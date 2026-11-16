//! Lotus MCP server binary.
//!
//! The server speaks MCP over stdio. It is local only: no TCP listener is
//! opened and no remote client can attach.

use lotus_mcp::LotusServer;
use lotus_session::supervisor::HostConfig;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = if let Some(path) = std::env::args_os().nth(1) {
        let config: HostConfig = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        LotusServer::configured(config, std::collections::BTreeMap::new())?
    } else {
        LotusServer::new()
    };
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
