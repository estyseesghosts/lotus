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
    InputArgs, InputPresetArgs, InputPresetOperation, KindArg, LoadArgs, MemoryArgs,
    PinArtifactArgs, RegistersArgs, ReleaseArtifactsArgs, SessionArgs, SettingsArgs,
    SettingsChangeArgs, SettingsOperation, SettingsSchemaArgs, ValidateScenarioArgs, WorkspaceArgs,
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
        description = "Report LOTUS frontend (headless/windowed), protocol, session tree, cards, approved asset handles, limits and the window workspace layout (workspace.available is false headless)."
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
            "workspace": host.map(Supervisor::workspace),
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

    #[tool(
        description = "Settings registry: every Lotus host/desktop and Azahar worker setting with label, group, type and bounds, default, scopes, apply class (live, renderer, boot, cfg_savedata, ...), availability per worker branch with exclusion reasons, and consumer citations. Optional scope: global, primary or secondary_runtime. Read-only."
    )]
    pub async fn lotus_settings_schema(
        &self,
        Parameters(args): Parameters<SettingsSchemaArgs>,
    ) -> CallToolResult {
        tool_result(Ok(
            lotus_settings::Registry::builtin().schema(args.scope.map(Into::into))
        ))
    }

    #[tool(
        description = "Effective worker settings and input layout keys (lotus.input.*) of a target (global, card, or session) with each value's source: default, global, card, runtime, or managed (host configuration profile, not editable). Lists ignored saved entries and unreadable files under problems. For a primary session, pending lists saved worker changes that apply at its card's next start."
    )]
    pub async fn lotus_settings(
        &self,
        Parameters(args): Parameters<SettingsArgs>,
    ) -> CallToolResult {
        let result = async { self.require_host()?.settings(&args.target.into()).await }.await;
        tool_result(result)
    }

    #[tool(
        description = "Change settings. Global and card overrides are saved and apply to sessions started afterwards, never to running workers. A secondary session's override is applied to its worker now and discarded when it ends; the worker refuses non-live settings while running. Input layout keys (lotus.input.*: preset, bind.<action>, stick dead zone/sensitivity/inversion, threshold) are never sent to a worker and reach running sessions' input routing at once, at every target. Primary sessions are changed through their card. Requires host control permission."
    )]
    pub async fn lotus_settings_change(
        &self,
        Parameters(args): Parameters<SettingsChangeArgs>,
    ) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            let target = args.target.into();
            let key = || {
                args.key
                    .as_deref()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "key is required"))
            };
            match args.operation {
                SettingsOperation::Set => {
                    let value = args.value.clone().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "value is required for set")
                    })?;
                    host.set_setting(&target, key()?, Some(value), Actor::Mcp)
                        .await?;
                }
                SettingsOperation::Unset => {
                    host.set_setting(&target, key()?, None, Actor::Mcp).await?;
                }
                SettingsOperation::Reset => host.reset_settings(&target, Actor::Mcp).await?,
            }
            host.settings(&target).await
        }
        .await;
        tool_result(result)
    }

    #[tool(
        description = "Input-layout preset library (lotus#63): list builtin and saved presets, export one as JSON, import exported JSON (a taken name gets a -N suffix), duplicate, rename or delete a saved preset. Builtin presets (default, xbox-labels) are read-only. Select one with the lotus.input.preset setting. Changes need host control permission."
    )]
    pub async fn lotus_input_presets(
        &self,
        Parameters(args): Parameters<InputPresetArgs>,
    ) -> CallToolResult {
        use lotus_session::settings_store::PresetChange;
        let result = (|| {
            let host = self.require_host()?;
            let need = |value: Option<String>, what: &str| {
                value.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("{what} is required"))
                })
            };
            let change = match args.operation {
                InputPresetOperation::List => {
                    let (saved, problems) = host.input_presets();
                    return Ok(serde_json::json!({
                        "builtin": lotus_input::layout::builtins()
                            .iter()
                            .map(|p| p.name.clone())
                            .collect::<Vec<_>>(),
                        "saved": saved.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                        "problems": problems,
                    }));
                }
                InputPresetOperation::Export => {
                    let json = host.export_input_preset(&need(args.name, "name")?)?;
                    return serde_json::from_str(&json).map_err(io::Error::other);
                }
                InputPresetOperation::Import => PresetChange::Import {
                    json: need(args.json, "json")?,
                },
                InputPresetOperation::Duplicate => PresetChange::Duplicate {
                    from: need(args.name, "name")?,
                    to: need(args.to, "to")?,
                },
                InputPresetOperation::Rename => PresetChange::Rename {
                    from: need(args.name, "name")?,
                    to: need(args.to, "to")?,
                },
                InputPresetOperation::Delete => PresetChange::Delete {
                    name: need(args.name, "name")?,
                },
            };
            let (name, warnings) = host.change_input_preset(change)?;
            Ok(serde_json::json!({
                "name": name,
                "warnings": warnings.iter().map(ToString::to_string).collect::<Vec<_>>(),
            }))
        })();
        tool_result(result)
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
        description = "Set complete buttons/touch/Circle Pad/C-stick state on an owned session (sticks need a worker listing analog_input); scenario ownership excludes live input. Requires control permission."
    )]
    pub async fn lotus_input(&self, Parameters(args): Parameters<InputArgs>) -> CallToolResult {
        let result = async {
            let host = self.require_host()?;
            let state = InputState {
                buttons: args.buttons,
                x: args.x,
                y: args.y,
                touch: args.touch,
                circle_pad: args.circle_pad,
                c_stick: args.c_stick,
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
        description = "Read or change what the LOTUS window shows (lotus#72), the same workspace the person changes in the window. operation.op: get (read-only), tile (two different sessions; axis auto/horizontal/vertical, ratio 0.2-0.8, screens per tile), single, untile (keep one), swap, set_axis, set_ratio, set_screen_layout, focus_viewport (keyboard target). A change commits all or nothing and returns the new revision; with expected_revision it is refused if the workspace changed since. Presentation only: never starts, stops or reconfigures a worker, the inspector target, audio focus or controllers. Changes need control permission. Headless: get reports available=false and every change fails as Unsupported."
    )]
    pub async fn lotus_workspace(
        &self,
        Parameters(args): Parameters<WorkspaceArgs>,
    ) -> CallToolResult {
        tool_result(self.require_host().and_then(|host| {
            let Some(change) = args.operation.change() else {
                return json(&host.workspace());
            };
            // The snapshot this change committed, not a later read another client may have
            // changed meanwhile.
            let snapshot = host.update_workspace(&change, args.expected_revision, Actor::Mcp)?;
            Ok(serde_json::json!({"committed": true, "workspace": snapshot}))
        }))
    }

    #[tool(
        description = "Make a session the LOTUS window's inspector target (inspector, gauges, audio). Outside a split the window also shows it; a split is never collapsed, reordered or changed, and controller assignment never moves. No effect when headless."
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

    /// lotus#50: the registry is readable without a host and matches the CLI output.
    #[tokio::test]
    async fn settings_schema_needs_no_host_and_filters_by_scope() {
        use crate::tool_arguments::{SettingsSchemaArgs, SettingsScope};
        let server = LotusServer::new();
        let read = |result: CallToolResult| -> Value {
            assert_ne!(result.is_error, Some(true));
            let text = serde_json::to_value(&result.content[0]).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_string();
            serde_json::from_str(&text).unwrap()
        };
        let all = read(
            server
                .lotus_settings_schema(Parameters(SettingsSchemaArgs::default()))
                .await,
        );
        assert_eq!(all, lotus_settings::Registry::builtin().schema(None));
        let secondary = read(
            server
                .lotus_settings_schema(Parameters(SettingsSchemaArgs {
                    scope: Some(SettingsScope::SecondaryRuntime),
                }))
                .await,
        );
        assert_eq!(
            secondary,
            lotus_settings::Registry::builtin()
                .schema(Some(lotus_settings::Scope::SecondaryRuntime))
        );
        assert!(!secondary.to_string().contains("lotus.desktop."));
    }

    /// lotus#63: the preset library through MCP; changes need control.
    #[tokio::test]
    async fn input_preset_tool_lists_copies_and_exports() {
        use crate::tool_arguments::{InputPresetArgs, InputPresetOperation};
        let root = std::env::temp_dir().join(format!("lotus-mcp-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("store")).unwrap();
        let worker = std::env::current_exe().unwrap();
        let args = |operation, name: Option<&str>, to: Option<&str>| InputPresetArgs {
            operation,
            name: name.map(str::to_string),
            to: to.map(str::to_string),
            json: None,
        };
        let call = |result: CallToolResult| -> (bool, Value) {
            let text = serde_json::to_value(&result.content[0]).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_string();
            (
                result.is_error == Some(true),
                serde_json::from_str(&text).unwrap_or(Value::String(text)),
            )
        };
        let read_only = LotusServer::configured(
            HostConfig::new(worker.clone(), root.join("store")),
            BTreeMap::new(),
        )
        .unwrap();
        let (error, message) = call(
            read_only
                .lotus_input_presets(Parameters(args(
                    InputPresetOperation::Duplicate,
                    Some("default"),
                    Some("mine"),
                )))
                .await,
        );
        assert!(
            error && message.to_string().contains("control"),
            "{message}"
        );
        drop(read_only);
        let mut config = HostConfig::new(worker, root.join("store"));
        config.allow_control = true;
        let server = LotusServer::configured(config, BTreeMap::new()).unwrap();
        let (error, value) = call(
            server
                .lotus_input_presets(Parameters(args(
                    InputPresetOperation::Duplicate,
                    Some("xbox-labels"),
                    Some("mine"),
                )))
                .await,
        );
        assert!(!error, "{value}");
        assert_eq!(value["name"], "mine");
        let (_, list) = call(
            server
                .lotus_input_presets(Parameters(args(InputPresetOperation::List, None, None)))
                .await,
        );
        assert_eq!(
            list["builtin"],
            serde_json::json!(["default", "xbox-labels"])
        );
        assert_eq!(list["saved"], serde_json::json!(["mine"]));
        let (_, exported) = call(
            server
                .lotus_input_presets(Parameters(args(
                    InputPresetOperation::Export,
                    Some("mine"),
                    None,
                )))
                .await,
        );
        assert_eq!(exported["version"], 1);
        assert!(exported["bindings"]["a"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!({"device": "button", "button": "south"})));
        let (error, _) = call(
            server
                .lotus_input_presets(Parameters(args(
                    InputPresetOperation::Delete,
                    Some("default"),
                    None,
                )))
                .await,
        );
        assert!(error, "builtin presets are read-only");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// lotus#51: settings reads and changes go through the supervisor's layered stores.
    #[tokio::test]
    async fn settings_tools_save_card_overrides_and_need_control() {
        use crate::tool_arguments::{
            SettingsArgs, SettingsChangeArgs, SettingsOperation, SettingsTargetArg,
        };
        let root = std::env::temp_dir().join(format!("lotus-mcp-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("store/cards/a")).unwrap();
        let worker = std::env::current_exe().unwrap();
        let change = |operation, value: Option<Value>| SettingsChangeArgs {
            target: SettingsTargetArg::Card { card: "a".into() },
            operation,
            key: Some("volume".into()),
            value,
        };
        let text = |result: CallToolResult| -> (bool, String) {
            let text = serde_json::to_value(&result.content[0]).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_string();
            (result.is_error == Some(true), text)
        };
        let read_only = LotusServer::configured(
            HostConfig::new(worker.clone(), root.join("store")),
            BTreeMap::new(),
        )
        .unwrap();
        let (error, message) = text(
            read_only
                .lotus_settings_change(Parameters(change(SettingsOperation::Set, Some(0.5.into()))))
                .await,
        );
        assert!(error && message.contains("control"), "{message}");
        drop(read_only);
        let mut config = HostConfig::new(worker, root.join("store"));
        config.allow_control = true;
        let server = LotusServer::configured(config, BTreeMap::new()).unwrap();
        let (error, message) = text(
            server
                .lotus_settings_change(Parameters(change(SettingsOperation::Set, Some(0.5.into()))))
                .await,
        );
        assert!(!error, "{message}");
        let shown: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(shown["settings"]["volume"]["source"], "card");
        let (_, message) = text(
            server
                .lotus_settings(Parameters(SettingsArgs {
                    target: SettingsTargetArg::Global,
                }))
                .await,
        );
        let shown: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(shown["settings"]["volume"]["source"], "default");
        let (error, _) = text(
            server
                .lotus_settings_change(Parameters(change(SettingsOperation::Set, None)))
                .await,
        );
        assert!(error, "set needs a value");
        let (error, message) = text(
            server
                .lotus_settings_change(Parameters(change(SettingsOperation::Unset, None)))
                .await,
        );
        assert!(!error, "{message}");
        let shown: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(shown["settings"]["volume"]["source"], "default");
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// lotus#67: a headless host says it has no window workspace.
    #[tokio::test]
    async fn headless_status_reports_no_workspace() {
        let root = std::env::temp_dir().join(format!("lotus-mcp-ws-{}", std::process::id()));
        let worker = std::env::current_exe().unwrap();
        let config = HostConfig::new(worker, root.join("store"));
        let server = LotusServer::configured(config, BTreeMap::new()).unwrap();
        let status: Value = serde_json::from_str(&server.lotus_status().await).unwrap();
        assert_eq!(status["workspace"]["available"], false);
        assert_eq!(status["workspace"]["revision"], 0);
        assert_eq!(status["workspace"]["layout"]["mode"], "empty");
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// lotus#72: `lotus_workspace` reads without a window, refuses changes on a
    /// headless host, and passes revision and lookup refusals through once enabled.
    #[tokio::test]
    async fn workspace_tool_reads_headless_and_refuses_changes_it_cannot_commit() {
        use crate::tool_arguments::{WorkspaceArgs, WorkspaceOperation};
        let root = std::env::temp_dir().join(format!("lotus-mcp-wst-{}", std::process::id()));
        let worker = std::env::current_exe().unwrap();
        let mut config = HostConfig::new(worker, root.join("store"));
        config.allow_control = true;
        let server = LotusServer::configured(config, BTreeMap::new()).unwrap();
        let call = |operation, expected_revision| {
            server.lotus_workspace(Parameters(WorkspaceArgs {
                operation,
                expected_revision,
            }))
        };
        let text = |result: CallToolResult| -> (bool, String) {
            let body = result.content[0].as_text().unwrap().text.clone();
            (result.is_error == Some(true), body)
        };
        let (failed, body) = text(call(WorkspaceOperation::Get, None).await);
        let read: Value = serde_json::from_str(&body).unwrap();
        assert!(!failed);
        assert_eq!(read["available"], false);
        let (failed, body) = text(call(WorkspaceOperation::Swap, None).await);
        assert!(failed && body.contains("no LOTUS window"), "{body}");

        server
            .require_host()
            .unwrap()
            .enable_workspace(Default::default());
        let tile = || WorkspaceOperation::Tile {
            first: "missing-a".into(),
            second: "missing-b".into(),
            axis: None,
            ratio: None,
            screens: None,
        };
        let (failed, body) = text(call(tile(), None).await);
        assert!(failed && body.contains("no session missing-a"), "{body}");
        let (failed, body) = text(call(WorkspaceOperation::Swap, Some(99)).await);
        assert!(failed && body.contains("expected revision 99"), "{body}");
        let (_, body) = text(call(WorkspaceOperation::Get, None).await);
        let read: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(read["available"], true);
        assert_eq!(read["revision"], 0);
        drop(server);
        let _ = std::fs::remove_dir_all(&root);
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

    /// MCP and `ctr-cli scenario validate` both call `validate_execution` (lotus#94).
    #[tokio::test]
    async fn scenario_validation_matches_the_cli() {
        let root = std::env::temp_dir().join(format!("lotus-mcp-scenario-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("temp dir");
        let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../specs/scenario-example.yaml");
        let text = std::fs::read_to_string(&example).expect("example");
        let azahar = root.join("azahar.yaml");
        std::fs::write(
            &azahar,
            text.replace("worker_flavor: experimental", "worker_flavor: integration"),
        )
        .expect("variant");
        let server = LotusServer {
            scenarios: BTreeMap::from([
                ("example".to_string(), example.clone()),
                ("azahar".to_string(), azahar.clone()),
            ]),
            ..LotusServer::new()
        };
        for (handle, path) in [("example", &example), ("azahar", &azahar)] {
            let cli = lotus_runner::execution::validate_execution(&Scenario::load(path).unwrap());
            let mcp = server
                .lotus_validate_scenario(Parameters(ValidateScenarioArgs {
                    path: handle.into(),
                }))
                .await;
            assert_eq!(mcp.is_error == Some(true), cli.is_err(), "{handle}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_defaults_to_disposable_secondary() {
        let args: CreateArgs = serde_json::from_str("{}").expect("defaults");
        assert_eq!(args.kind, KindArg::Secondary);
        assert!(serde_json::from_str::<CreateArgs>(r#"{"path":"x"}"#).is_err());
    }
}
