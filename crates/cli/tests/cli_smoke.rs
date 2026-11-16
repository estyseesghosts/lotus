//! Smoke test for the command line interface.
//!
//! The CLI is the headless entry point of the project. It must run without a
//! GUI session and it must report the pinned revisions.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ctr-cli"))
}

#[test]
fn version_reports_the_protocol_version() {
    let output = cli().arg("version").output().expect("cannot run ctr-cli");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("worker protocol 1.0"),
        "unexpected output: {stdout}"
    );
}

#[test]
fn pins_reports_the_pinned_revisions() {
    let output = cli().arg("pins").output().expect("cannot run ctr-cli");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("azahar"), "unexpected output: {stdout}");
    assert!(stdout.contains("gpui"), "unexpected output: {stdout}");
}

#[test]
fn unknown_commands_fail() {
    let output = cli()
        .arg("definitely-not-a-command")
        .output()
        .expect("cannot run ctr-cli");
    assert!(!output.status.success());
}
