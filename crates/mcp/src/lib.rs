//! Model Context Protocol server for the Lotus session supervisor.
//!
//! The MCP server is an adapter, not a second debugger. Every tool it
//! exposes goes through the same typed session operations that the desktop
//! application and the CLI use. It is local only: there is no remote listener
//! and no unauthenticated RPC endpoint.

use lotus_runner::Scenario;
use lotus_session::SessionManager;
use lotus_trace::EventCategory;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Arguments of the scenario validation tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ValidateScenarioArgs {
    /// Path of the scenario file, relative to the working directory of the
    /// server or absolute.
    pub path: String,
}

/// The MCP server.
#[derive(Clone)]
pub struct LotusServer {
    tool_router: ToolRouter<Self>,
    sessions: Arc<Mutex<SessionManager>>,
    storage_root: PathBuf,
}

impl LotusServer {
    /// Create a server with an empty session registry.
    pub fn new() -> Self {
        Self::with_storage_root(default_storage_root())
    }

    /// Create a server that keeps session storage below `storage_root`.
    pub fn with_storage_root(storage_root: impl Into<PathBuf>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            sessions: Arc::new(Mutex::new(SessionManager::new())),
            storage_root: storage_root.into(),
        }
    }

    /// Storage root used by this server.
    pub fn storage_root(&self) -> &std::path::Path {
        &self.storage_root
    }

    /// Number of sessions currently known to the supervisor.
    pub fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .map(|manager| manager.len())
            .unwrap_or(0)
    }
}

impl Default for LotusServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = tool_router)]
impl LotusServer {
    /// Report the state of the Lotus server.
    #[tool(
        description = "Report the Lotus server status: worker protocol version, session count and event categories."
    )]
    pub async fn lotus_status(&self) -> String {
        let status = serde_json::json!({
            "protocol_version": lotus_protocol::protocol_id(),
            "sessions": self.session_count(),
            "storage_root": self.storage_root.display().to_string(),
            "event_categories": event_category_ids(),
        });
        status.to_string()
    }

    /// List the sessions known to the supervisor.
    #[tool(description = "List the emulator sessions currently known to the Lotus supervisor.")]
    pub async fn lotus_list_sessions(&self) -> String {
        let ids = match self.sessions.lock() {
            Ok(manager) => manager
                .ids()
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        serde_json::json!({ "sessions": ids }).to_string()
    }

    /// Validate a scenario file.
    #[tool(
        description = "Load a scenario file and report whether it satisfies the project's execution limits."
    )]
    pub async fn lotus_validate_scenario(
        &self,
        Parameters(args): Parameters<ValidateScenarioArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = PathBuf::from(&args.path);
        match Scenario::load(&path) {
            Ok(scenario) => match scenario.validate() {
                Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::json!({
                        "path": args.path,
                        "valid": true,
                        "name": scenario.name,
                        "max_frames": scenario.stop.max_frames,
                    })
                    .to_string(),
                )])),
                Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "scenario is invalid: {error}"
                ))])),
            },
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "cannot load scenario: {error}"
            ))])),
        }
    }

    /// List the event categories the trace layer records.
    #[tool(description = "List the structured event categories the Lotus trace layer can record.")]
    pub async fn lotus_event_categories(&self) -> String {
        serde_json::json!({ "categories": event_category_ids() }).to_string()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for LotusServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Lotus exposes the emulator session supervisor to agents. Tools are local only and \
             operate on sessions created by the host application. Raw traces and captures are \
             returned as artifact references, never as inline payloads.",
        )
    }
}

/// Identifiers of every known event category.
fn event_category_ids() -> Vec<&'static str> {
    vec![
        EventCategory::Worker.id(),
        EventCategory::GpuSubmitCmdList.id(),
        EventCategory::GpuDraw.id(),
        EventCategory::Cpu.id(),
        EventCategory::Memory.id(),
        EventCategory::ServiceCall.id(),
        EventCategory::KernelIpc.id(),
        EventCategory::Exception.id(),
    ]
}

/// Default location of the per-session storage roots.
fn default_storage_root() -> PathBuf {
    std::env::temp_dir().join("lotus-sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn status_reports_protocol_version() {
        let server = LotusServer::with_storage_root(std::env::temp_dir().join("lotus-mcp-test"));
        let status = server.lotus_status().await;
        assert!(status.contains(&lotus_protocol::protocol_id()));
    }

    #[tokio::test]
    async fn event_categories_are_listed() {
        let server = LotusServer::with_storage_root(std::env::temp_dir().join("lotus-mcp-test"));
        let categories = server.lotus_event_categories().await;
        assert!(categories.contains("gpu.draw"));
    }

    #[tokio::test]
    async fn missing_scenario_is_reported_as_an_error_result() {
        let server = LotusServer::with_storage_root(std::env::temp_dir().join("lotus-mcp-test"));
        let args = Parameters(ValidateScenarioArgs {
            path: "does-not-exist.yaml".to_string(),
        });
        let result = server.lotus_validate_scenario(args).await.unwrap();
        assert_eq!(result.is_error, Some(true));
    }
}
