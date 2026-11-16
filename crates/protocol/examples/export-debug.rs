fn main() -> std::io::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::write(
        root.join("specs/debug-v1.schema.json"),
        serde_json::to_string_pretty(&lotus_protocol::debug_schema::schema())? + "\n",
    )?;
    std::fs::write(
        root.join("docs/32-debugger-command-parity.md"),
        lotus_protocol::debug_schema::parity(),
    )
}
