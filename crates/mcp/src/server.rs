//! MCP tools over a shared [`Supervisor`].
//!
//! The same server runs headless (`lotus-mcp`) or inside the desktop
//! (`lotus-desktop --mcp`); both frontends then drive the same sessions with
//! the same tools. Every per-session tool takes the session ID, so sessions
//! are addressed independently and run concurrently.
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use lotus_runner::Scenario;
use lotus_session::native_session::InputState;
use lotus_session::supervisor::{Artifact, HostConfig, SessionKind, Supervisor};
use lotus_session::telemetry::{Actor, GUEST_REFRESH_HZ};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler};
use serde_json::Value;

use crate::tool_arguments::{
    AdvanceArgs, ControlArgs, ControlOperation, CreateArgs, DebugArgs, GdbArgs, GdbOperation,
    InputArgs, KindArg, LoadArgs, MemoryArgs, PinArtifactArgs, RegistersArgs, ReleaseArtifactsArgs,
    SessionArgs, ValidateScenarioArgs,
};

/// Which frontend hosts this server; reported to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frontend {
    Headless,
    Windowed,
}

impl Frontend {
    fn name(self) -> &'static str {
        match self {
            Frontend::Headless => "headless",
            Frontend::Windowed => "windowed",
        }
    }
}

#[derive(Clone)]
pub struct LotusServer {
    tool_router: ToolRouter<Self>,
    host: Option<Arc<Supervisor>>,
    frontend: Frontend,
    gdb: Arc<tokio::sync::Mutex<BTreeMap<String, lotus_gdb::Server>>>,
    scenarios: BTreeMap<String, PathBuf>,
}

impl Default for LotusServer {
    fn default() -> Self {
        Self::new()
    }
}

impl LotusServer {
    /// Status-only server: cannot spawn workers or read files.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            host: None,
            gdb: Arc::default(),
            frontend: Frontend::Headless,
            scenarios: BTreeMap::new(),
        }
    }

    /// Headless server owning a new supervisor.
    pub fn configured(
        config: HostConfig,
        scenarios: BTreeMap<String, PathBuf>,
    ) -> io::Result<Self> {
        Ok(Self::shared(
            Arc::new(Supervisor::new(config)?),
            Frontend::Headless,
            scenarios,
        ))
    }

    /// Server over a supervisor another frontend also drives.
    #[must_use]
    pub fn shared(
        host: Arc<Supervisor>,
        frontend: Frontend,
        scenarios: BTreeMap<String, PathBuf>,
    ) -> Self {
        Self {
            tool_router: Self::tool_router(),
            host: Some(host),
            gdb: Arc::default(),
            frontend,
            scenarios,
        }
    }

    #[must_use]
    pub fn host(&self) -> Option<&Arc<Supervisor>> {
        self.host.as_ref()
    }

    pub async fn resource_bytes(&self, uri: &str) -> io::Result<Vec<u8>> {
        self.require_host()?.read_artifact(uri)
    }

    fn require_host(&self) -> io::Result<&Supervisor> {
        self.host.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native host is not configured",
            )
        })
    }
}

fn tool_result(value: io::Result<Value>) -> CallToolResult {
    match value {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
    }
}

fn json<T: serde::Serialize>(value: &T) -> io::Result<Value> {
    serde_json::to_value(value).map_err(io::Error::other)
}

fn resource(artifact: &Artifact) -> Resource {
    Resource::new(&artifact.uri, &artifact.screen)
        .with_mime_type(&artifact.mime_type)
        .with_size(artifact.bytes as u64)
        .with_description(format!(
            "{} {}x{}, SHA-256 {}",
            artifact.mime_type, artifact.width, artifact.height, artifact.sha256
        ))
}

#[tool_router(router = tool_router)]
impl LotusServer {
    #[tool(
        description = "Start or stop an opt-in loopback GDB server for this session. Port 0 selects a free port. GDB exclusively leases mutation control while attached."
    )]
    async fn lotus_gdb(&self, Parameters(args): Parameters<GdbArgs>) -> CallToolResult {
        tool_result(
            async {
                let host = self
                    .host
                    .clone()
                    .ok_or_else(|| io::Error::other("native host is not configured"))?;
                host.require_control()?;
                host.summary(&args.session)?;
                let mut servers = self.gdb.lock().await;
                match args.operation {
                    GdbOperation::Start => {
                        if servers.contains_key(&args.session) {
                            return Err(io::Error::other(
                                "GDB server already started for this session",
                            ));
                        }
                        let server =
                            lotus_gdb::Server::start(host, args.session.clone(), args.port).await?;
                        let address = server.address.to_string();
                        servers.insert(args.session, server);
                        Ok(serde_json::json!({"address": address, "loopback_only": true}))
                    }
                    GdbOperation::Stop => {
                        servers.remove(&args.session);
                        Ok(serde_json::json!({"stopped": true}))
                    }
                }
            }
            .await,
        )
    }

    #[tool(
        description = "Report LOTUS frontend (headless/windowed), protocol, session tree, cards, approved asset handles and limits."
    )]
    pub async fn lotus_status(&self) -> String {
        let host = self.host.as_deref();
        serde_json::json!({
            "protocol_version": lotus_protocol::protocol_id(),
            "frontend": self.frontend.name(),
            "control": host.is_some_and(Supervisor::allows_control),
            "sessions": host.map(Supervisor::summaries).unwrap_or_default(),
            "cards": host.and_then(|host| host.cards().ok()).unwrap_or_default(),
            "mounted_cards": host.map(Supervisor::mounted_cards).unwrap_or_default(),
            "assets": host.map(Supervisor::assets).unwrap_or_default(),
            "unavailable_asset_directories": host.map(Supervisor::unavailable_asset_directories).unwrap_or_default(),
            "max_sessions": host.map(Supervisor::max_sessions).unwrap_or(0),
            "guest_refresh_hz": GUEST_REFRESH_HZ,
            "frontend_metrics": host.map(Supervisor::frontend_metrics).unwrap_or_default(),
            "artifacts": host.map(Supervisor::artifact_usage),
            "policy": host.map(Supervisor::policy_status),
        })
        .to_string()
    }

    #[tool(
        description = "List owned sessions: id, kind (primary/secondary), card or parent, loaded asset, and gauges (state, playing, fps, speed, frames, audio, memory, last actor). Never waits on a busy worker."
    )]
    pub async fn lotus_list_sessions(&self) -> CallToolResult {
        tool_result(self.require_host().and_then(|host| json(&host.summaries())))
    }

    #[tool(
        description = "List persistent cards (primary SD/NAND roots) and which session mounts each."
    )]
    pub async fn lotus_list_cards(&self) -> CallToolResult {
        tool_result(self.require_host().and_then(|host| {
            Ok(serde_json::json!({
                "cards": host.cards()?,
                "mounted": host.mounted_cards(),
            }))
        }))
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
        description = "Start a session in its own worker process. Handles expire when this server exits; primary cards retain guest storage across restarts. kind=primary mounts a persistent card (one primary per card); kind=secondary (default) is disposable: empty, or a point-in-time copy of `card`, deleted on stop. Optional asset loads paused. Requires host control permission."
    )]
    pub async fn lotus_create_session(
        &self,
        Parameters(args): Parameters<CreateArgs>,
    ) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            let kind = match (args.kind, args.card) {
                (KindArg::Primary, Some(card)) => SessionKind::Primary { card },
                (KindArg::Primary, None) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a primary session needs a card",
                    ))
                }
                (KindArg::Secondary, parent) => SessionKind::Secondary { parent },
            };
            let info = host.create(kind, args.asset.as_deref(), Actor::Mcp).await?;
            Ok(serde_json::json!({"session": info.id, "info": info}))
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "Load an approved asset into an owned session, replacing its content; leaves it paused. Requires control permission."
    )]
    pub async fn lotus_load(&self, Parameters(args): Parameters<LoadArgs>) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            json(&host.load(&args.session, &args.asset, Actor::Mcp).await?)
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "Advance an owned session by exactly 1–600 frames and leave it paused (20-second native deadline). Stops continuous running."
    )]
    pub async fn lotus_run_until(
        &self,
        Parameters(args): Parameters<AdvanceArgs>,
    ) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            host.advance(&args.session, args.frames, Actor::Mcp).await
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "run (continuous, returns immediately), pause, reset or stop an owned session. Requires host control permission."
    )]
    pub async fn lotus_control(&self, Parameters(args): Parameters<ControlArgs>) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            let id = &args.session;
            match args.operation {
                ControlOperation::Run => {
                    host.run(id, Actor::Mcp).await?;
                    Ok(serde_json::json!({"playing": true}))
                }
                ControlOperation::Pause => host.pause(id, Actor::Mcp).await,
                ControlOperation::Reset => host.reset(id, Actor::Mcp).await,
                ControlOperation::Stop => {
                    host.stop(id, Actor::Mcp).await?;
                    self.gdb.lock().await.remove(id);
                    Ok(serde_json::json!({"state": "stopped"}))
                }
            }
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "Query an owned worker at a safe point; returns worker status plus gauges."
    )]
    pub async fn lotus_session_status(
        &self,
        Parameters(args): Parameters<SessionArgs>,
    ) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            let status = host.status(&args.session).await?;
            let summary = host.summary(&args.session)?;
            Ok(serde_json::json!({"status": status, "session": summary}))
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "Inspect settings with consumer sources, full CPU/VFP registers, threads/waits, processes, memory map, profiler, and frame-pointer backtraces. CITnLO supports paused register/memory editing and named checkpoints checked against the source pin and settings. Commands are typed and bounded; mutations require host control permission."
    )]
    pub async fn lotus_debug(&self, Parameters(args): Parameters<DebugArgs>) -> CallToolResult {
        let result = async {
            let result = self
                .require_host()?
                .debug(&args.session, &args.command, Actor::Mcp)
                .await?;
            if matches!(args.command, lotus_protocol::debug::DebugCommand::Stop) {
                self.gdb.lock().await.remove(&args.session);
            }
            Ok(result)
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "Read r0–r15 and CPSR of one CPU core at a safe point. Values describe the instant of the reply only."
    )]
    pub async fn lotus_registers(
        &self,
        Parameters(args): Parameters<RegistersArgs>,
    ) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            host.registers(&args.session, args.core).await
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "Read 1–4096 mapped bytes from an owned worker at a safe point. No writes."
    )]
    pub async fn lotus_read_memory(
        &self,
        Parameters(args): Parameters<MemoryArgs>,
    ) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            host.memory(&args.session, args.address, args.length).await
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "Set complete buttons/touch state on an owned session; scenario ownership excludes live input. Requires control permission."
    )]
    pub async fn lotus_input(&self, Parameters(args): Parameters<InputArgs>) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            let state = InputState {
                buttons: args.buttons,
                x: args.x,
                y: args.y,
                touch: args.touch,
            };
            host.input(&args.session, state, &args.owner, Actor::Mcp)
                .await
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "Capture two native screens; return immutable bounded artifact resources, not inline pixels."
    )]
    pub async fn lotus_capture(&self, Parameters(args): Parameters<SessionArgs>) -> CallToolResult {
        let result = async { self.require_host()?.capture(&args.session).await }.await;
        match result {
            Ok(artifacts) => CallToolResult::success(
                artifacts
                    .iter()
                    .map(|artifact| ContentBlock::resource_link(resource(artifact)))
                    .collect(),
            ),
            Err(error) => tool_result(Err(error)),
        }
    }

    #[tool(
        description = "Delete artifacts now and free their quota. Evicted or released URIs later read as expired. Requires control permission."
    )]
    pub async fn lotus_release_artifacts(
        &self,
        Parameters(args): Parameters<ReleaseArtifactsArgs>,
    ) -> CallToolResult {
        tool_result(self.require_host().and_then(|host| {
            let released = host.release_artifacts(&args.uris)?;
            Ok(serde_json::json!({"released": released, "usage": host.artifact_usage()}))
        }))
    }

    #[tool(
        description = "Pin an artifact so eviction never removes it (a session may pin up to its guaranteed share, see lotus_status.artifacts.floor_*), or unpin it. New artifacts are otherwise leased for lease_seconds, then evictable least-recently-used first. Requires control permission."
    )]
    pub async fn lotus_pin_artifact(
        &self,
        Parameters(args): Parameters<PinArtifactArgs>,
    ) -> CallToolResult {
        tool_result(
            self.require_host()
                .and_then(|host| json(&host.pin_artifact(&args.uri, args.pinned)?)),
        )
    }

    #[tool(
        description = "Show an owned session in the LOTUS window so a person can watch it. No effect when headless."
    )]
    pub async fn lotus_focus_session(
        &self,
        Parameters(args): Parameters<SessionArgs>,
    ) -> CallToolResult {
        tool_result(self.require_host().and_then(|host| {
            host.request_focus(&args.session)?;
            Ok(serde_json::json!({"frontend": self.frontend.name()}))
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for LotusServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_instructions("LOTUS owns isolated AZAHAR/CITnLO sessions, each its own worker process, addressed by session ID and run concurrently. Primary sessions persist a card's SD/NAND; secondary sessions are disposable copies. Only configured asset/session handles are accepted. Host permission controls mutations. Captures are local resources; no shell or arbitrary filesystem tools. In a windowed LOTUS the person sees the same sessions.")
    }

    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = self
            .host
            .as_deref()
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
        let mime = self
            .require_host()
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?
            .artifacts()
            .into_iter()
            .find(|a| a.uri == request.uri)
            .map(|a| a.mime_type)
            .unwrap_or_else(|| "application/octet-stream".into());
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(ReadResourceResult::new(vec![
            ResourceContents::blob(encoded, request.uri).with_mime_type(mime)
        ])
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mcp_schema_exposes_the_entire_shared_debug_command_union() {
        let schema = serde_json::to_value(schemars::schema_for!(DebugArgs)).expect("tool schema");
        let actual = &schema["$defs"]["DebugCommand"]["oneOf"];
        let expected = &lotus_protocol::debug_schema::schema()["oneOf"];
        assert_eq!(
            actual, expected,
            "a CLI/MCP debug operation was omitted from the public schema"
        );
        assert!(actual.is_array());
    }

    #[tokio::test]
    async fn unconfigured_server_cannot_read_paths_or_spawn_processes() {
        let server = LotusServer::new();
        let status = server.lotus_status().await;
        assert!(status.contains(&lotus_protocol::protocol_id()));
        assert!(status.contains("\"frontend\":\"headless\""));
        assert!(server
            .resource_bytes("file:///C:/Windows/win.ini")
            .await
            .is_err());
        let result = server
            .lotus_create_session(Parameters(CreateArgs {
                kind: KindArg::Secondary,
                card: None,
                asset: Some("C:/Windows/notepad.exe".into()),
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

    #[test]
    fn create_defaults_to_disposable_secondary() {
        let args: CreateArgs = serde_json::from_str("{}").expect("defaults");
        assert_eq!(args.kind, KindArg::Secondary);
        assert!(serde_json::from_str::<CreateArgs>(r#"{"path":"x"}"#).is_err());
    }
}
