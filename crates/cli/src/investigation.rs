//! Bridges to the shared desktop host's MCP tools (Windows): investigations and recorded
//! tests act on the desktop's live sessions, so the CLI calls the desktop rather than
//! starting a host of its own.
use anyhow::{bail, Context, Result};
use lotus_mcp::tool_arguments::{InvestigationOperation, RecordTestOperation};
use rmcp::{model::CallToolRequestParams, transport::TokioChildProcess, ServiceExt};
use serde_json::{Map, Value};
use std::{io::BufRead, path::PathBuf, time::Duration};

pub fn run(args: &[String]) -> Result<()> {
    let [host, request @ ..] = args else {
        bail!("usage: ctr-cli investigation <host-config> [operation-json]");
    };
    bridge(host, request, "lotus_investigation", Map::new(), |line| {
        let operation: InvestigationOperation = serde_json::from_str(line)
            .context("expected a lotus_investigation operation object")?;
        Ok(serde_json::to_value(operation)?)
    })
}

/// `ctr-cli record-test <host-config> <session> [operation-json]` (lotus#95).
pub fn record_test(args: &[String]) -> Result<()> {
    let [host, session, request @ ..] = args else {
        bail!("usage: ctr-cli record-test <host-config> <session> [operation-json]");
    };
    let mut fixed = Map::new();
    fixed.insert("session".into(), Value::String(session.clone()));
    bridge(host, request, "lotus_record_test", fixed, |line| {
        let operation: RecordTestOperation =
            serde_json::from_str(line).context("expected a lotus_record_test operation object")?;
        Ok(serde_json::to_value(operation)?)
    })
}

/// Call `tool` once with `request`, or once per stdin line, adding `fixed` arguments.
fn bridge(
    host: &str,
    request: &[String],
    tool: &'static str,
    fixed: Map<String, Value>,
    parse: impl Fn(&str) -> Result<Value>,
) -> Result<()> {
    if request.len() > 1 {
        bail!("expected at most one JSON operation");
    }
    let one_shot = request.first().map(|arg| parse(arg)).transpose()?;
    let desktop = std::env::current_exe()?.with_file_name("lotus-desktop.exe");
    let desktop = std::env::var_os("LOTUS_DESKTOP").map_or(desktop, PathBuf::from);
    let mut bridge = tokio::process::Command::new(&desktop);
    bridge.args(["--mcp", host]);
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let transport = TokioChildProcess::new(bridge)
            .with_context(|| format!("cannot launch {}", desktop.display()))?;
        let client = tokio::time::timeout(Duration::from_secs(30), ().serve(transport))
            .await
            .context("shared desktop MCP handshake timed out")??;
        let call = |operation: Value| {
            let mut arguments = fixed.clone();
            arguments.insert("operation".into(), operation);
            let client = &client;
            async move {
                // A recorded-test start reloads content and a mark captures both screens.
                let result = tokio::time::timeout(
                    Duration::from_secs(60),
                    client.call_tool(CallToolRequestParams::new(tool).with_arguments(arguments)),
                )
                .await
                .with_context(|| format!("{tool} request timed out"))??;
                let reply = result
                    .content
                    .iter()
                    .find_map(|block| block.as_text())
                    .with_context(|| format!("missing {tool} response"))?;
                if result.is_error == Some(true) {
                    bail!("{}", reply.text);
                }
                Ok(serde_json::from_str::<Value>(&reply.text)?)
            }
        };
        if let Some(operation) = one_shot {
            println!("{}", serde_json::to_string_pretty(&call(operation).await?)?);
        } else {
            for line in std::io::stdin().lock().lines() {
                let reply = match parse(&line?) {
                    Ok(operation) => call(operation).await,
                    Err(error) => Err(error),
                };
                match reply {
                    Ok(result) => println!("{}", serde_json::json!({"result": result})),
                    Err(error) => println!("{}", serde_json::json!({"error": error.to_string()})),
                }
            }
        }
        client.cancel().await?;
        Ok(())
    })
}
