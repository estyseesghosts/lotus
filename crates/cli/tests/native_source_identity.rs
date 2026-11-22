//! `ctr-cli debug` binds ELF identity through the same supervisor path as the
//! desktop and MCP (lotus#76). Opt-in: needs a native worker and the owned fixture.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn nm_address(elf: &Path, name: &str) -> u32 {
    let output = Command::new("llvm-nm").arg(elf).output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let line = text
        .lines()
        .find(|line| line.ends_with(&format!(" {name}")))
        .unwrap();
    u32::from_str_radix(&line[..8], 16).unwrap()
}

fn addr2line(elf: &Path, address: u32) -> u32 {
    let output = Command::new("llvm-addr2line")
        .arg("-e")
        .arg(elf)
        .arg(format!("{address:#x}"))
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let line = text.trim().rsplit_once(':').unwrap().1;
    line.split_whitespace().next().unwrap().parse().unwrap()
}

#[test]
#[ignore = "requires LOTUS_NATIVE_WORKER, LOTUS_DEBUG_FIXTURE and LOTUS_DEBUG_FIXTURE_GCC with adjacent ELFs"]
fn cli_binds_only_the_loaded_build() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").unwrap());
    let fixture = PathBuf::from(std::env::var_os("LOTUS_DEBUG_FIXTURE").unwrap());
    let gcc = PathBuf::from(std::env::var_os("LOTUS_DEBUG_FIXTURE_GCC").unwrap());
    let elf = fixture.with_extension("elf");
    let root = std::env::temp_dir().join(format!("lotus-cli-source-id-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let config = root.join("host.json");
    std::fs::write(
        &config,
        json!({
            "worker": worker,
            "storage_root": root.join("host"),
            "allow_control": true,
            "assets": {"fixture": fixture, "elf": elf, "gcc_elf": gcc.with_extension("elf")},
        })
        .to_string(),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ctr-cli"))
        .arg("debug")
        .arg(&config)
        .arg("fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let info: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert!(info["id"].is_string(), "{info}");
    let mut request = |command: Value| -> Value {
        writeln!(stdin, "{command}").unwrap();
        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
    };
    request(json!({"operation": "pause"}));
    let refused = request(json!({"operation": "load_symbols", "asset": "gcc_elf"}));
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("differs from guest memory"),
        "{refused}"
    );
    let bound = request(json!({"operation": "load_symbols", "asset": "elf"}));
    assert_eq!(bound["mapping"], "verified_bytes", "{bound}");
    let thumb_tick = nm_address(&elf, "thumb_tick");
    let resolved = request(json!({"operation": "resolve_symbol", "query": "thumb_tick"}));
    assert_eq!(resolved["addresses"][0], thumb_tick);
    let tick = nm_address(&elf, "tick");
    let located = request(json!({"operation": "symbolize", "addresses": [tick, thumb_tick]}));
    for (index, address) in [tick, thumb_tick].into_iter().enumerate() {
        assert_eq!(
            located["locations"][index]["line"],
            addr2line(&elf, address),
            "{located}"
        );
    }
    assert_eq!(located["image"]["process"], bound["process"]);
    request(json!({"operation": "reset"}));
    let stale = request(json!({"operation": "symbolize", "addresses": [tick]}));
    assert!(
        stale["error"]
            .as_str()
            .unwrap()
            .contains("ELF binding dropped: guest epoch"),
        "{stale}"
    );
    writeln!(stdin, "quit").unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    std::fs::remove_dir_all(root).unwrap();
}

/// Line of the `// @name` marker in the owned C++ source fixture.
fn marker(name: &str) -> u32 {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/source/fixture.cpp");
    let tag = format!("// @{name}");
    std::fs::read_to_string(source)
        .unwrap()
        .lines()
        .position(|line| line.trim_end().ends_with(&tag))
        .unwrap() as u32
        + 1
}

#[test]
#[ignore = "requires LOTUS_NATIVE_WORKER and LOTUS_SOURCE_FIXTURE with its adjacent ELF"]
fn cli_sets_source_breakpoints_and_steps_lines() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").unwrap());
    let fixture = PathBuf::from(std::env::var_os("LOTUS_SOURCE_FIXTURE").unwrap());
    let elf = fixture.with_extension("elf");
    let root = std::env::temp_dir().join(format!("lotus-cli-source-step-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let sources = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/source");
    let config = root.join("host.json");
    std::fs::write(
        &config,
        json!({
            "worker": worker,
            "storage_root": root.join("host"),
            "allow_control": true,
            "assets": {"fixture": fixture, "elf": elf},
            "source_roots": {"fixture": {"path": sources}},
        })
        .to_string(),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ctr-cli"))
        .arg("debug")
        .arg(&config)
        .arg("fixture")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let info: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert!(info["id"].is_string(), "{info}");
    let mut request = |command: Value| -> Value {
        writeln!(stdin, "{command}").unwrap();
        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
    };
    request(json!({"operation": "pause"}));
    request(json!({"operation": "load_symbols", "asset": "elf"}));
    let line = marker("frame_first");
    let set =
        request(json!({"operation": "source_breakpoint_set", "file": "fixture.cpp", "line": line}));
    assert_eq!(set["state"], "armed", "{set}");
    let breakpoint = set["sites"][0]["breakpoint"].clone();
    request(json!({"operation": "continue"}));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = request(json!({"operation": "status"}));
        if status["paused"] == true && status["stop"]["breakpoint"] == breakpoint {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{status}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let step = request(
        json!({"operation": "source_step", "core": 0, "mode": "over", "max_instructions": 10000}),
    );
    assert_eq!(step["reason"], "statement", "{step}");
    assert_eq!(step["location"]["line"], marker("frame_call_thumb"));
    let pc = step["location"]["pc"].as_u64().unwrap() as u32;
    assert_eq!(addr2line(&elf, pc), marker("frame_call_thumb"));
    // With no `from` prefix the DWARF path itself must lie below the root.
    let excerpt = request(
        json!({"operation": "source_excerpt", "file": "fixture.cpp", "line": line, "context": 0}),
    );
    assert_eq!(excerpt["checksum"], "md5_verified", "{excerpt}");
    assert!(excerpt["lines"][0]["text"]
        .as_str()
        .unwrap()
        .contains("// @frame_first"));
    writeln!(stdin, "quit").unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    std::fs::remove_dir_all(root).unwrap();
}
