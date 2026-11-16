//! `ctr-cli`: headless access to the Lotus tooling.
//!
//! The CLI shares the same typed operations as the desktop application and
//! the MCP server. It never opens a window and never requires a GUI session.

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
  ctr-cli version              Print the Lotus version and worker protocol version
  ctr-cli pins                 Print the pinned Azahar and GPUI revisions
  ctr-cli scenario validate <path>
                                Validate a scenario file against the execution limits
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    match command {
        "version" => {
            println!("lotus {}", env!("CARGO_PKG_VERSION"));
            println!("worker protocol {}", protocol_id());
        }
        "pins" => {
            let pins = load_pins()?;
            println!("{}", toml::to_string_pretty(&pins)?);
        }
        "scenario" => {
            let sub = args.get(1).map(String::as_str).unwrap_or("help");
            match sub {
                "validate" => {
                    let path = args
                        .get(2)
                        .context("usage: ctr-cli scenario validate <path>")?;
                    let scenario = Scenario::load(path)
                        .with_context(|| format!("cannot load scenario {path}"))?;
                    scenario
                        .validate()
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
