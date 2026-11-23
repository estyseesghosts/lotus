//! `ctr-cli`: headless access to the Lotus tooling.
//!
//! The CLI shares the same typed operations as the desktop application and
//! the MCP server. It never opens a window and never requires a GUI session.

#[cfg(windows)]
mod investigation;

use anyhow::{bail, Context, Result};
use lotus_protocol::protocol_id;
use lotus_runner::Scenario;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Pinned revisions, mirrored from `config/pins.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pins {
    azahar: AzaharPins,
    gpui: GpuiPins,
    rust: RustPins,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AzaharPins {
    repository: String,
    fork: String,
    upstream_commit: String,
    integration_branch: String,
    integration_commit: String,
    experimental_branch: String,
    experimental_commit: String,
    vendor_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GpuiPins {
    repository: String,
    rev: String,
    crate_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RustPins {
    channel: String,
    host: String,
}

/// Usage text.
const USAGE: &str = "\
ctr-cli: Lotus command line interface

Usage:
  ctr-cli debug <host-config> <asset-or-dash> [--gdb <port>]    Typed JSON-line debugger on stdin; replies on stdout
  ctr-cli investigation <host-config> [operation-json] (Windows shared desktop)
                                Read or edit investigations. Without JSON, accept operations on stdin.
  ctr-cli record-test <host-config> <session> [operation-json] (Windows shared desktop)
                                Record a desktop session as a scenario test: {\"op\":\"start\"},
                                mark, stop, status, discard, export. Without JSON, read stdin.
  ctr-cli version              Print the Lotus version and worker protocol version
  ctr-cli mock <worker-executable> Query a mock worker over private pipes
  ctr-cli pins                 Print the pinned Azahar and GPUI revisions
  ctr-cli settings schema [global|primary|secondary]
                                Print the settings registry, optionally only one scope's keys
  ctr-cli settings show|set|unset|reset <host-config> global|card:<name> [key] [json]
                                Read or change saved global defaults or card overrides;
                                changes need allow_control in the host configuration
  ctr-cli scenario validate <path>
                                Validate a scenario file against the execution limits
  ctr-cli scenario run <path> <worker> <artifact-root> [repeat-count]
                                Execute bounded native runs (default: 3)
  ctr-cli scenario assess <path> <worker> <artifact-root> [repeat-count]
                                Retain a repeat assessment and all attempts (default: 3)
  ctr-cli suite run <manifest> <worker> <artifact-root> <source-revision>
  ctr-cli suite show <artifact-root> <suite-id>
  ctr-cli suite list <artifact-root>
  ctr-cli suite detail <artifact-root> <suite-id>
  ctr-cli suite compare <artifact-root> <baseline-id> <candidate-id>
  ctr-cli suite export <artifact-root> <baseline-id> <candidate-id> <new-output-directory>
  ctr-cli suite bisect <host-config> <plan.json> (Windows)
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    match command {
        #[cfg(windows)]
        "investigation" => investigation::run(&args[1..])?,
        #[cfg(windows)]
        "record-test" => investigation::record_test(&args[1..])?,
        "debug-schema" => println!(
            "{}",
            serde_json::to_string_pretty(&lotus_protocol::debug_schema::schema())?
        ),
        "settings" if args.get(1).is_some_and(|a| a != "schema") => {
            use lotus_session::supervisor::{HostConfig, SettingsTarget, Supervisor};
            use lotus_session::telemetry::Actor;
            const USAGE: &str = "usage: ctr-cli settings show|set|unset|reset <host-config> \
                                 global|card:<name> [key] [json-value]";
            let operation = args[1].as_str();
            let config: HostConfig =
                serde_json::from_str(&std::fs::read_to_string(args.get(2).context(USAGE)?)?)?;
            let target = match args.get(3).map(String::as_str).context(USAGE)? {
                "global" => SettingsTarget::Global,
                other => match other.strip_prefix("card:") {
                    Some(card) => SettingsTarget::Card { card: card.into() },
                    None => bail!("{USAGE}"),
                },
            };
            let key = args.get(4);
            tokio::runtime::Runtime::new()?.block_on(async {
                let host = Supervisor::new(config)?;
                let changes = match operation {
                    "show" => None,
                    "set" => {
                        let value: serde_json::Value =
                            serde_json::from_str(args.get(5).context(USAGE)?)
                                .context("the value must be JSON, e.g. 100, true or \"text\"")?;
                        Some(
                            host.set_setting(
                                &target,
                                key.context(USAGE)?,
                                Some(value),
                                Actor::Host,
                            )
                            .await?,
                        )
                    }
                    "unset" => Some(
                        host.set_setting(&target, key.context(USAGE)?, None, Actor::Host)
                            .await?,
                    ),
                    "reset" => Some(host.reset_settings(&target, Actor::Host).await?),
                    _ => bail!("{USAGE}"),
                };
                let mut report = host.settings(&target).await?;
                if let Some(changes) = changes {
                    report["changes"] = serde_json::to_value(changes)?;
                }
                println!("{}", serde_json::to_string_pretty(&report)?);
                anyhow::Ok(())
            })?;
        }
        "settings" => {
            if args.get(1).map(String::as_str) != Some("schema") {
                bail!("usage: ctr-cli settings schema [global|primary|secondary]");
            }
            let scope = match args.get(2).map(String::as_str) {
                None => None,
                Some("global") => Some(lotus_settings::Scope::Global),
                Some("primary") => Some(lotus_settings::Scope::Primary),
                Some("secondary") => Some(lotus_settings::Scope::SecondaryRuntime),
                Some(other) => bail!("unknown scope {other:?}"),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&lotus_settings::Registry::builtin().schema(scope))?
            );
        }
        "version" => {
            println!("lotus {}", env!("CARGO_PKG_VERSION"));
            println!("worker protocol {}", protocol_id());
        }
        "pins" => {
            let pins = load_pins()?;
            println!("{}", toml::to_string_pretty(&pins)?);
        }
        "mock" => {
            let executable = args
                .get(1)
                .context("usage: ctr-cli mock <worker-executable>")?;
            let root = std::env::temp_dir().join(format!(
                "lotus-cli-{}",
                lotus_protocol::envelope::SessionUuid::generate()
            ));
            let spec = lotus_session::WorkerSpec::new(executable, &root);
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(async {
                let mut worker = lotus_session::controlled_worker::ControlledWorker::spawn(
                    &spec,
                    std::time::Duration::from_secs(3),
                )
                .await?;
                println!("{}", serde_json::to_string(&worker.status().await?)?);
                worker.stop().await
            })?;
        }
        "debug" => {
            use lotus_session::supervisor::{HostConfig, SessionKind, Supervisor};
            use lotus_session::telemetry::Actor;
            use std::io::BufRead;
            let config: HostConfig = serde_json::from_str(&std::fs::read_to_string(
                args.get(1).context("debug requires a host configuration")?,
            )?)?;
            let asset = args
                .get(2)
                .context("debug requires an asset handle or '-'")?;
            tokio::runtime::Runtime::new()?.block_on(async {
                let host = std::sync::Arc::new(Supervisor::new(config)?);
                let info = host
                    .create(
                        SessionKind::Secondary { parent: None },
                        if asset == "-" {
                            None
                        } else {
                            Some(asset.as_str())
                        },
                        Actor::Host,
                    )
                    .await?;
                println!("{}", serde_json::to_string(&info)?);
                let gdb = if args.get(3).is_some_and(|arg| arg == "--gdb") {
                    let port = args
                        .get(4)
                        .context("--gdb requires port (0 selects a free port)")?
                        .parse()?;
                    let server =
                        lotus_gdb::Server::start(host.clone(), info.id.clone(), port).await?;
                    eprintln!("GDB listening on {}", server.address);
                    Some(server)
                } else {
                    None
                };
                for line in std::io::stdin().lock().lines() {
                    let line = line?;
                    if line.trim() == "quit" {
                        break;
                    }
                    let reply =
                        match serde_json::from_str::<lotus_protocol::debug::DebugCommand>(&line) {
                            Ok(command) => host
                                .debug(&info.id, &command, Actor::Host)
                                .await
                                .unwrap_or_else(|e| serde_json::json!({"error": e.to_string()})),
                            Err(e) => serde_json::json!({"error": e.to_string()}),
                        };
                    println!("{reply}");
                }
                if let Some(gdb) = gdb {
                    if let Err(error) = gdb.stop().await {
                        eprintln!("GDB stop: {error}");
                    }
                }
                host.close().await;
                Ok::<(), anyhow::Error>(())
            })?;
        }
        "suite" => suite(&args[1..])?,
        "scenario" => {
            let sub = args.get(1).map(String::as_str).unwrap_or("help");
            match sub {
                "run" | "assess" => {
                    let path = PathBuf::from(args.get(2).with_context(|| {
                        format!("scenario {sub} requires path, worker and artifact root")
                    })?);
                    let worker = PathBuf::from(args.get(3).context("missing worker executable")?)
                        .canonicalize()?;
                    let output = PathBuf::from(args.get(4).context("missing artifact root")?);
                    let count = args
                        .get(5)
                        .map(|value| value.parse::<u8>())
                        .transpose()?
                        .unwrap_or(3);
                    let scenario = Scenario::load(&path)?;
                    let directory = path.parent().unwrap_or(std::path::Path::new("."));
                    let runtime = tokio::runtime::Runtime::new()?;
                    if sub == "assess" {
                        let assessment = runtime.block_on(lotus_runner::assessment::assess(
                            &scenario, directory, &worker, &output, count,
                        ))?;
                        println!("{}", serde_json::to_string_pretty(&assessment)?);
                        if assessment.status != lotus_runner::assessment::Status::Pass {
                            bail!(
                                "scenario assessment {:?}; see {}",
                                assessment.status,
                                output.join(assessment.id).join("assessment.json").display()
                            );
                        }
                        return Ok(());
                    }
                    let reports = runtime.block_on(lotus_runner::execution::repeat(
                        &scenario, directory, &worker, &output, count,
                    ))?;
                    println!("{}", serde_json::to_string_pretty(&reports)?);
                }
                "validate" => {
                    let path = args
                        .get(2)
                        .context("usage: ctr-cli scenario validate <path>")?;
                    let scenario = Scenario::load(path)
                        .with_context(|| format!("cannot load scenario {path}"))?;
                    // Same check as MCP lotus_validate_scenario: schema plus executor support.
                    lotus_runner::execution::validate_execution(&scenario)
                        .with_context(|| format!("scenario {path} is invalid"))?;
                    println!("scenario {} is valid", scenario.name);
                }
                _ => {
                    println!("{USAGE}");
                    bail!("unknown scenario subcommand: {sub}");
                }
            }
        }
        "help" | "--help" | "-h" => println!("{USAGE}"),
        other => {
            println!("{USAGE}");
            bail!("unknown command: {other}");
        }
    }

    Ok(())
}

fn suite(args: &[String]) -> Result<()> {
    use lotus_runner::{assessment::Status, history, suite};
    let report = match args {
        #[cfg(windows)]
        [command, config, plan] if command == "bisect" => {
            use lotus_session::supervisor::{HostConfig, Supervisor};
            use std::{path::Path, sync::Arc};
            let config: HostConfig = serde_json::from_slice(&std::fs::read(config)?)?;
            let host = Arc::new(Supervisor::new(config)?);
            let report = tokio::runtime::Runtime::new()?.block_on(async {
                let result = lotus_runner::bisect::run(&host, Path::new(plan)).await;
                host.close().await;
                result
            })?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if let Some(reason) = report.reason {
                bail!("bisect {} unresolved: {reason}", report.id);
            }
            return Ok(());
        }
        [command, manifest, worker, output, revision] if command == "run" => {
            let report = tokio::runtime::Runtime::new()?.block_on(suite::run(
                std::path::Path::new(manifest),
                std::path::Path::new(worker),
                std::path::Path::new(output),
                revision,
            ))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report.status != Status::Pass {
                bail!("suite {} finished with {:?}", report.id, report.status);
            }
            return Ok(());
        }
        [command, output, id] if command == "show" => {
            suite::show(std::path::Path::new(output), id)?
        }
        [command, output] if command == "list" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&history::list(std::path::Path::new(output))?)?
            );
            return Ok(());
        }
        [command, output, id] if command == "detail" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&history::detail(std::path::Path::new(output), id)?)?
            );
            return Ok(());
        }
        [command, output, baseline, candidate] if command == "compare" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&history::compare(
                    std::path::Path::new(output),
                    baseline,
                    candidate
                )?)?
            );
            return Ok(());
        }
        [command, history, baseline, candidate, output] if command == "export" => {
            lotus_runner::repro::export(
                std::path::Path::new(history),
                baseline,
                candidate,
                std::path::Path::new(output),
            )?;
            return Ok(());
        }
        _ => {
            bail!("usage: ctr-cli suite <run|show|list|detail|compare|export|bisect>; see ctr-cli --help")
        }
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Load the pinned revision manifest.
fn load_pins() -> Result<Pins> {
    let path = pins_path();
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
}

/// Location of the pinned revision manifest.
fn pins_path() -> PathBuf {
    // The manifest lives in the repository root. Walk up from the crate
    // directory so the CLI works from a source checkout and from a build
    // directory inside the repository.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut root = manifest.as_path();
    while let Some(parent) = root.parent() {
        if parent.join("config").join("pins.toml").is_file() {
            return parent.join("config").join("pins.toml");
        }
        root = parent;
    }
    manifest.join("../../config/pins.toml")
}
