//! Headless LOTUS: the MCP server over stdio, no window.
//!
//! Local only: no TCP listener is opened and no remote client can attach. When
//! the client disconnects every session is stopped, so secondary data is
//! discarded and primary cards are unlocked.

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
    // Each session supervises itself (lotus_session::supervisor); there is no host pump.
    let host = server.host().cloned();
    let service = server.serve(rmcp::transport::stdio()).await?;
    let result = service.waiting().await;
    if let Some(host) = host {
        let outcomes = host.close().await;
        let failed: Vec<String> = outcomes
            .iter()
            .filter_map(|(id, result)| result.as_ref().err().map(|e| format!("{id}: {e}")))
            .collect();
        eprintln!(
            "LOTUS closed {} session(s), {} not cleanly; primary cards retained, secondary guest data discarded. Session handles expire with this server.",
            outcomes.len(),
            failed.len()
        );
        for line in failed {
            eprintln!("  {line}");
        }
    }
    result?;
    Ok(())
}
