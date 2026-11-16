//! Headless LOTUS: the MCP server over stdio, no window.
//!
//! Local only: no TCP listener is opened and no remote client can attach. When
//! the client disconnects every session is stopped, so secondary data is
//! discarded and primary cards are unlocked.

use std::time::Duration;

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
    let host = server.host().cloned();
    let pump = host.clone().map(|host| {
        tokio::spawn(async move {
            loop {
                host.pump().await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    });
    let service = server.serve(rmcp::transport::stdio()).await?;
    let result = service.waiting().await;
    if let Some(pump) = pump {
        pump.abort();
    }
    if let Some(host) = host {
        let count = host.sessions().len();
        host.close().await;
        eprintln!("LOTUS closed {count} session(s); primary cards retained, secondary guest data discarded. Session handles expire with this server.");
    }
    result?;
    Ok(())
}
