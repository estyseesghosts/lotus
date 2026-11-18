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
fn settings_schema_lists_registry_groups_by_scope() {
    let schema = |scope: Option<&str>| -> serde_json::Value {
        let mut command = cli();
        command.args(["settings", "schema"]);
        command.args(scope);
        let output = command.output().expect("cannot run ctr-cli");
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).expect("json")
    };
    let count = |schema: &serde_json::Value| -> usize {
        schema["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["settings"].as_array().unwrap().len())
            .sum()
    };
    let all = schema(None);
    assert_eq!(all["version"], 1);
    let primary = schema(Some("primary"));
    assert!(
        count(&primary) < count(&all),
        "excluded and host keys drop out"
    );
    assert!(!primary.to_string().contains("lotus.host."));
    assert!(!cli()
        .args(["settings", "schema", "nowhere"])
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn settings_are_saved_through_the_host_configuration() {
    let dir = std::env::temp_dir().join(format!("lotus-cli-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("store/cards/a")).unwrap();
    let config = dir.join("host.json");
    let write_config = |allow_control: bool| {
        std::fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                // Any existing file: no worker starts for settings commands.
                "worker": env!("CARGO_BIN_EXE_ctr-cli"),
                "storage_root": dir.join("store"),
                "allow_control": allow_control,
            }))
            .unwrap(),
        )
        .unwrap();
    };
    let run = |args: &[&str]| cli().arg("settings").args(args).output().unwrap();
    let config_arg = config.to_str().unwrap();
    write_config(false);
    assert!(!run(&["set", config_arg, "global", "volume", "0.5"])
        .status
        .success());
    write_config(true);
    let output = run(&["set", config_arg, "card:a", "volume", "0.5"]);
    assert!(output.status.success(), "{output:?}");
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown["settings"]["volume"]["source"], "card");
    let output = run(&["show", config_arg, "global"]);
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown["settings"]["volume"]["source"], "default");
    let output = run(&["unset", config_arg, "card:a", "volume"]);
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(shown["settings"]["volume"]["source"], "default");
    assert!(!run(&["set", config_arg, "card:a", "volume", "loud"])
        .status
        .success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_commands_fail() {
    let output = cli()
        .arg("definitely-not-a-command")
        .output()
        .expect("cannot run ctr-cli");
    assert!(!output.status.success());
}
