use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use lotus_runner::Scenario;
use lotus_session::native_session::InputState;
use lotus_session::supervisor::{Artifact, HostConfig, Supervisor};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler};
use tokio::sync::Mutex;

use crate::tool_arguments::{
    AdvanceArgs, ControlArgs, ControlOperation, CreateArgs, InputArgs, MemoryArgs, SessionArgs,
    ValidateScenarioArgs,
};

#[derive(Clone)]
pub struct LotusServer {
    tool_router: ToolRouter<Self>,
    host: Arc<Mutex<Option<Supervisor>>>,
    storage_root: PathBuf,
    scenarios: BTreeMap<String, PathBuf>,
}

impl Default for LotusServer {
    fn default() -> Self {
        Self::new()
    }
}
impl LotusServer {
    #[must_use]
    pub fn new() -> Self {
        Self::with_storage_root(std::env::temp_dir().join("lotus-sessions"))
    }
    #[must_use]
    pub fn with_storage_root(root: impl Into<PathBuf>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            host: Arc::new(Mutex::new(None)),
            storage_root: root.into(),
            scenarios: BTreeMap::new(),
        }
    }
    pub fn configured(
        config: HostConfig,
        scenarios: BTreeMap<String, PathBuf>,
    ) -> io::Result<Self> {
        let root = config.storage_root.clone();
        Ok(Self {
            tool_router: Self::tool_router(),
            host: Arc::new(Mutex::new(Some(Supervisor::new(config)?))),
            storage_root: root,
            scenarios,
        })
    }
    #[must_use]
    pub fn storage_root(&self) -> &std::path::Path {
        &self.storage_root
    }
    pub async fn resource_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
        self.host
            .lock()
            .await
            .as_ref()
            .ok_or_else(unconfigured)?
            .read_artifact(uri)
    }
}
fn unconfigured() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "native host is not configured",
    )
}
fn tool_result(value: io::Result<serde_json::Value>) -> CallToolResult {
    match value {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
    }
}
fn resource(artifact: &Artifact) -> Resource {
    Resource::new(&artifact.uri, &artifact.screen)
        .with_mime_type("application/octet-stream")
        .with_size(artifact.bytes as u64)
        .with_description(format!(
            "Native BGRA8 {}x{}, SHA-256 {}",
            artifact.width, artifact.height, artifact.sha256
        ))
}

#[tool_router(router = tool_router)]
impl LotusServer {
    #[tool(description = "Report LOTUS protocol, owned sessions and approved asset handles.")]
    pub async fn lotus_status(&self) -> String {
        let guard = self.host.lock().await;
        serde_json::json!({"protocol_version": lotus_protocol::protocol_id(),
            "sessions": guard.as_ref().map(Supervisor::sessions).unwrap_or_default(),
            "assets": guard.as_ref().map(Supervisor::assets).unwrap_or_default(), "max_workers": 4})
        .to_string()
    }
    #[tool(description = "List only this client's owned session handles.")]
    pub async fn lotus_list_sessions(&self) -> String {
        self.lotus_status().await
    }
    #[tool(description = "Validate a host-approved scenario handle; no filesystem paths accepted.")]
    pub async fn lotus_validate_scenario(
        &self,
        Parameters(args): Parameters<ValidateScenarioArgs>,
    ) -> CallToolResult {
        let Some(path) = self.scenarios.get(&args.path) else {
            return tool_result(Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unknown scenario handle",
            )));
        };
        tool_result(
            Scenario::load(path)
                .map_err(io::Error::other)
                .and_then(|scenario| {
                    lotus_runner::execution::validate_execution(&scenario)?;
                    Ok(serde_json::json!({"valid": true, "name": scenario.name}))
                }),
        )
    }
    #[tool(
        description = "Create and load a private native session from an approved asset handle. Requires host control permission."
    )]
    pub async fn lotus_create_session(
        &self,
        Parameters(args): Parameters<CreateArgs>,
    ) -> CallToolResult {
        let mut guard = self.host.lock().await;
        tool_result(
            async {
                let host = guard.as_mut().ok_or_else(unconfigured)?;
                Ok(serde_json::json!({"session": host.create(&args.asset).await?}))
            }
            .await,
        )
    }
    #[tool(
        description = "Advance an owned session by 1–600 frames, with a 20-second native deadline."
    )]
    pub async fn lotus_run_until(
        &self,
        Parameters(args): Parameters<AdvanceArgs>,
    ) -> CallToolResult {
        let mut guard = self.host.lock().await;
        tool_result(
            async {
                let host = guard.as_mut().ok_or_else(unconfigured)?;
                host.require_control()?;
                host.session(&args.session)?.advance(args.frames).await
            }
            .await,
        )
    }
    #[tool(
        description = "Pause, reset or stop an owned session. Requires host control permission."
    )]
    pub async fn lotus_control(&self, Parameters(args): Parameters<ControlArgs>) -> CallToolResult {
        let mut guard = self.host.lock().await;
        tool_result(
            async {
                let host = guard.as_mut().ok_or_else(unconfigured)?;
                host.require_control()?;
                match args.operation {
                    ControlOperation::Pause => host.session(&args.session)?.pause().await,
                    ControlOperation::Reset => host.session(&args.session)?.reset().await,
                    ControlOperation::Stop => {
                        host.stop(&args.session).await?;
                        Ok(serde_json::json!({"state": "stopped"}))
                    }
                }
            }
            .await,
        )
    }
    #[tool(description = "Query an owned worker at a safe point.")]
    pub async fn lotus_session_status(
        &self,
        Parameters(args): Parameters<SessionArgs>,
    ) -> CallToolResult {
        let mut guard = self.host.lock().await;
        tool_result(
            async {
                guard
                    .as_mut()
                    .ok_or_else(unconfigured)?
                    .session(&args.session)?
                    .status()
                    .await
            }
            .await,
        )
    }
    #[tool(
        description = "Read 1–4096 mapped bytes from an owned worker at a safe point. No writes."
    )]
    pub async fn lotus_read_memory(
        &self,
        Parameters(args): Parameters<MemoryArgs>,
    ) -> CallToolResult {
        let mut guard = self.host.lock().await;
        tool_result(
            async {
                guard
                    .as_mut()
                    .ok_or_else(unconfigured)?
                    .session(&args.session)?
                    .memory(args.address, args.length)
                    .await
            }
            .await,
        )
    }

    #[tool(
        description = "Set complete buttons/touch state on an owned session; scenario ownership excludes live input. Requires control permission."
    )]
    pub async fn lotus_input(&self, Parameters(args): Parameters<InputArgs>) -> CallToolResult {
        let mut guard = self.host.lock().await;
        tool_result(
            async {
                let host = guard.as_mut().ok_or_else(unconfigured)?;
                host.require_control()?;
                host.session(&args.session)?
                    .input(
                        InputState {
                            buttons: args.buttons,
                            x: args.x,
                            y: args.y,
                            touch: args.touch,
                        },
                        &args.owner,
                    )
                    .await
            }
            .await,
        )
    }
    #[tool(
        description = "Capture two native screens; return immutable bounded artifact resources, not inline pixels."
    )]
    pub async fn lotus_capture(&self, Parameters(args): Parameters<SessionArgs>) -> CallToolResult {
        let mut guard = self.host.lock().await;
        match async {
            guard
                .as_mut()
                .ok_or_else(unconfigured)?
                .capture(&args.session)
                .await
        }
        .await
        {
            Ok(artifacts) => CallToolResult::success(
                artifacts
                    .iter()
                    .map(|artifact| ContentBlock::resource_link(resource(artifact)))
                    .collect(),
            ),
            Err(error) => tool_result(Err(error)),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for LotusServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_instructions("LOTUS owns isolated AZAHAR/CITnLO sessions. Only configured asset/session handles are accepted. Host permission controls mutations. Captures are local resources; no shell or arbitrary filesystem tools.")
    }
    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let guard = self.host.lock().await;
        let resources = guard
            .as_ref()
            .map(Supervisor::artifacts)
            .unwrap_or_default()
            .iter()
            .map(resource)
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let bytes = self
            .resource_bytes(&request.uri)
            .await
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(ReadResourceResult::new(vec![
            ResourceContents::blob(encoded, request.uri).with_mime_type("application/octet-stream")
        ])
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unconfigured_server_cannot_read_paths_or_spawn_processes() {
        let server = LotusServer::new();
        assert!(server
            .lotus_status()
            .await
            .contains(&lotus_protocol::protocol_id()));
        assert!(server
            .resource_bytes("file:///C:/Windows/win.ini")
            .await
            .is_err());
        let result = server
            .lotus_create_session(Parameters(CreateArgs {
                asset: "C:/Windows/notepad.exe".into(),
            }))
            .await;
        assert_eq!(result.is_error, Some(true));
        let result = server
            .lotus_validate_scenario(Parameters(ValidateScenarioArgs {
                path: "../../config/pins.toml".into(),
            }))
            .await;
        assert_eq!(result.is_error, Some(true));
    }
}
