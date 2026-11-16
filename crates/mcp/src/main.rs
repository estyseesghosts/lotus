//! Lotus MCP server binary.
//!
//! The server speaks MCP over stdio. It is local only: no TCP listener is
//! opened and no remote client can attach.

use lotus_mcp::LotusServer;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = LotusServer::new();
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
