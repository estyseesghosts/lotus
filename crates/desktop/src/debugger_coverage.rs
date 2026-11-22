use std::collections::BTreeSet;
use std::fmt::Write as _;

use lotus_mcp::LotusServer;
use lotus_protocol::{capabilities::feature, debug_schema};

pub(crate) const INVENTORY: &str = include_str!("debugger_coverage.tsv");

pub fn report() -> Result<String, String> {
    render(INVENTORY)
}

fn render(inventory: &str) -> Result<String, String> {
    let mut rows = [Vec::new(), Vec::new(), Vec::new()];
    let mut names = [BTreeSet::new(), BTreeSet::new()];
    for (index, line) in inventory.lines().enumerate().skip(1) {
        let fields = line.split('\t').collect::<Vec<_>>();
        let [surface, name, route, permission, required_feature, detail, verification] =
            fields.as_slice()
        else {
            return Err(format!(
                "debugger_coverage.tsv:{} needs seven columns",
                index + 1
            ));
        };
        let section = match *surface {
            "debug" => 0,
            "mcp-debug" => 1,
            "mcp" => 2,
            _ => return Err(format!("unknown coverage surface {surface}")),
        };
        if !names[usize::from(section != 0)].insert((*name).to_owned()) {
            return Err(format!("duplicate {surface} coverage for {name}"));
        }
        let route_link = match *route {
            "inspector polling" | "inspector rows" => Some("inspection.rs"),
            "inspector actions" | "session controls" | "workspace" => Some("lotus_window.rs"),
            "CPU inspector" | "session status" => Some("desktop_controller.rs"),
            "desktop input" => Some("input_routing.rs"),
            "settings window" => Some("settings_window.rs"),
            "investigations window" => Some("investigation_window.rs"),
            "debug bridge" if section == 1 => None,
            "palette" => Some("lotus_window/command_palette.rs"),
            "Builds inspector" => Some("lotus_window/builds_panel.rs"),
            "GDB inspector" => Some("lotus_window/gdb_panel.rs"),
            "GPU capture form" => Some("lotus_window/gpu_capture.rs"),
            "Memory inspector" => Some("lotus_window/memory_panel.rs"),
            "Memory search" => Some("lotus_window/memory_search.rs"),
            "Snapshots inspector" => Some("lotus_window/snapshots_panel.rs"),
            "Event timeline" => Some("lotus_window/event_timeline.rs"),
            "Kernel explorer" => Some("lotus_window/kernel_panel.rs"),
            "Breakpoint manager" => Some("lotus_window/breakpoints.rs"),
            "external only" | "unavailable" if *detail != "-" => None,
            _ => {
                return Err(format!(
                    "unknown or unexplained desktop route {route} for {name}"
                ))
            }
        };
        if *required_feature != "-"
            && !feature::ALL.contains(
                &required_feature
                    .split_whitespace()
                    .next()
                    .unwrap_or_default(),
            )
        {
            return Err(format!(
                "unknown worker feature {required_feature} for {name}"
            ));
        }
        let verification_link = match *verification {
            "CPU" | "Memory search" | "Snapshots" | "Timeline" | "Kernel" | "Source identity"
            | "Source debugging" | "Variables" | "Builds" => Some("tests/native_inspector.rs"),
            "Memory" | "Breakpoints" => Some("src/lotus_window/breakpoints.rs"),
            "Guest tools" => Some("src/lotus_window/command_palette.rs"),
            "GPU"
            | "GDB"
            | "Session"
            | "Peripherals"
            | "Peripherals (refusal)"
            | "Peripherals (mount and refusal)" => Some("tests/native_inspector.rs"),
            "Workspace" => Some("tests/responsive_controller.rs"),
            "Settings" => Some("src/settings_window/shared.rs"),
            "Input" => Some("tests/input_routing.rs"),
            "Investigations" => Some("../../tests/test_shared_desktop_mcp.py"),
            "external" if *route == "external only" => None,
            "fixture pending" if *route != "external only" => None,
            _ => {
                return Err(format!(
                    "unknown or inconsistent verification {verification} for {name}"
                ))
            }
        };
        let verification = verification_link.map_or_else(
            || (*verification).to_owned(),
            |file| format!("[{verification}](../crates/desktop/{file})"),
        );
        let route = route_link.map_or_else(
            || (*route).to_owned(),
            |file| format!("[{route}](../crates/desktop/src/{file})"),
        );
        rows[section].push((
            *name,
            format!("| `{name}` | {route} | {permission} | {required_feature} | {detail} | {verification} |"),
        ));
    }

    let expected = [
        debug_schema::operations(),
        LotusServer::new().registered_tool_names(),
    ];
    for (section, expected) in expected.into_iter().enumerate() {
        let expected = expected.into_iter().collect::<BTreeSet<_>>();
        let missing = expected.difference(&names[section]).collect::<Vec<_>>();
        let obsolete = names[section].difference(&expected).collect::<Vec<_>>();
        if !missing.is_empty() || !obsolete.is_empty() {
            return Err(format!(
                "update debugger_coverage.tsv for {}: unclassified {missing:?}, obsolete {obsolete:?}",
                ["DebugCommand", "MCP router"][section]
            ));
        }
    }

    let mut text = String::from(concat!(
        "# 39 — Desktop debugger coverage\n\n",
        "Regenerate with `cargo run -p lotus-desktop --example export-debugger-coverage`.\n",
        "The [coverage test](../crates/desktop/src/debugger_coverage.rs) checks every operation ",
        "from the [shared schema](../specs/debug-v1.schema.json) and the ",
        "[MCP tool router](../crates/mcp/src/server.rs).\n\n",
        "Linked desktop routes may expose a subset of arguments. The ",
        "[command palette](../crates/desktop/src/lotus_window/command_palette.rs) also builds ",
        "argument forms from the shared schema and lists external-only operations with their reason. The ",
        "[Run Clipboard JSON action](../crates/desktop/src/lotus_window.rs) accepts shared commands ",
        "subject to normal host and worker checks. Permissions describe those checks.\n\n",
        "Route coverage is independent of native acceptance. The host checks ",
        "[command requirements](../crates/protocol/src/debug.rs) against the selected worker's ",
        "negotiated features and forwards the worker's own refusal. A dash in the feature column ",
        "means the shared command declares no additional feature gate. It carries no backend support guarantee.\n\n",
        "Verification links to the corresponding desktop test family. GPU and GDB exercises ",
        "include shared desktop/MCP calls on packaged workers. Memory, Breakpoints, ",
        "and Guest tools include mounted interaction with CITnLO. CPU includes an owned ARM/Thumb fixture. ",
        "Source identity binds clang and devkitARM builds of that fixture and checks refusals of other builds. ",
        "Source debugging sets line breakpoints and steps lines on clang and devkitARM builds of a C++ fixture, ",
        "checked against llvm-addr2line and llvm-objdump. ",
        "Variables reads locals, arguments, globals and expressions of -O1 clang and devkitARM builds of a C fixture ",
        "and compares them with the values the fixture defines. ",
        "Builds exercises fixture rebuilds and two-session isolation through the window host and compares desktop and MCP status. ",
        "Workspace and Input use mock workers. ",
        "Fixture pending means the route still needs mapped desktop acceptance. This inventory ",
        "records test coverage independently of the outcome of any release run.\n"
    ));
    for (header, mut section) in [
        "Shared debugger commands",
        "MCP debugger tools",
        "MCP host tools",
    ]
    .into_iter()
    .zip(rows)
    {
        section.sort_unstable_by_key(|(name, _)| *name);
        writeln!(
            text,
            "\n## {header}\n\n| Operation | Desktop route | Permission | Extra worker feature | Coverage notes | Verification |\n| --- | --- | --- | --- | --- | --- |"
        ).expect("write to String");
        for (_, row) in section {
            writeln!(text, "{row}").expect("write to String");
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    #[test]
    fn debugger_coverage_matches_schema_router_and_checked_in_report() {
        let actual = super::report().expect("classify every registered operation");
        assert_eq!(
            include_str!("../../../docs/39-desktop-debugger-coverage.md"),
            actual,
            "run cargo run -p lotus-desktop --example export-debugger-coverage"
        );
    }

    #[test]
    fn an_operation_requires_a_verification_classification() {
        let changed = super::INVENTORY.replace(
            "\tCPU inspector\tread\t-\t-\tCPU",
            "\tCPU inspector\tread\t-\t-\t",
        );
        let error = super::render(&changed).unwrap_err();
        assert!(error.contains("unknown or inconsistent verification"));
    }

    #[test]
    fn missing_and_renamed_operations_report_both_differences() {
        let changed = super::INVENTORY
            .replace("debug\tadvance\t", "debug\told_advance\t")
            .replace("mcp-debug\tlotus_gdb\t", "mcp-debug\told_gdb\t");
        let error = super::render(&changed).unwrap_err();
        assert!(
            error.contains(r#"DebugCommand: unclassified ["advance"], obsolete ["old_advance"]"#)
        );
        let error = super::render(&changed.replace("debug\told_advance\t", "debug\tadvance\t"))
            .unwrap_err();
        assert!(error.contains(r#"MCP router: unclassified ["lotus_gdb"], obsolete ["old_gdb"]"#));
    }
}
