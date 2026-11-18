//! Headless LOTUS: the MCP server over stdio, no window.
//!
//! Local only: no TCP listener is opened and no remote client can attach. When
//! the client disconnects every session is stopped, so secondary data is
//! discarded and primary cards are unlocked.

use lotus_mcp::LotusServer;
use lotus_session::supervisor::{parse_asset_dir, HostConfig};
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `lotus-mcp [host.json] [--asset-dir handle=path ...]`; overrides replace the same handle.
    let mut config_path = None;
    let mut asset_dirs = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--asset-dir" {
            let value = arguments.next().ok_or("--asset-dir expects handle=path")?;
            asset_dirs.push(parse_asset_dir(&value)?);
        } else if config_path.is_none() {
            config_path = Some(argument);
        } else {
            return Err("usage: lotus-mcp [host.json] [--asset-dir handle=path ...]".into());
        }
    }
    let server = if let Some(path) = config_path {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read host configuration {path}: {error}"))?;
        let mut config: HostConfig = serde_json::from_str(&text)?;
        config.asset_directories.extend(asset_dirs);
        LotusServer::configured(config, std::collections::BTreeMap::new())?
    } else if asset_dirs.is_empty() {
        LotusServer::new()
    } else {
        return Err("--asset-dir needs a host.json".into());
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
