use anyhow::{bail, Context, Result};
use lotus_mcp::tool_arguments::InvestigationOperation;
use rmcp::{model::CallToolRequestParams, transport::TokioChildProcess, ServiceExt};
use std::{io::BufRead, path::PathBuf, time::Duration};

pub fn run(args: &[String]) -> Result<()> {
    let [host, request @ ..] = args else {
        bail!("usage: ctr-cli investigation <host-config> [operation-json]");
    };
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
        let call = |operation: InvestigationOperation| async {
            let mut arguments = serde_json::Map::new();
            arguments.insert("operation".into(), serde_json::to_value(operation)?);
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                client.call_tool(
                    CallToolRequestParams::new("lotus_investigation").with_arguments(arguments),
                ),
            )
            .await
            .context("investigation request timed out")??;
            let reply = result
                .content
                .iter()
                .find_map(|block| block.as_text())
                .context("missing investigation response")?;
            if result.is_error == Some(true) {
                bail!("{}", reply.text);
            }
            Ok(serde_json::from_str::<serde_json::Value>(&reply.text)?)
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

fn parse(line: &str) -> Result<InvestigationOperation> {
    serde_json::from_str(line).context("expected a lotus_investigation operation object")
}
