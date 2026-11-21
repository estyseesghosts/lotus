fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let report = lotus_desktop::debugger_coverage::report()?;
    std::fs::write(root.join("docs/39-desktop-debugger-coverage.md"), report)?;
    Ok(())
}
