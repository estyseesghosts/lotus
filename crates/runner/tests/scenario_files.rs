//! Every scenario file in the repository must load and validate.
//!
//! A scenario that does not validate is a broken contract, not a runtime
//! failure to be discovered later.

use lotus_runner::Scenario;
use std::path::PathBuf;

fn scenario_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/scenarios")
        .to_path_buf()
}

#[test]
fn every_scenario_in_the_repository_validates() {
    let directory = scenario_directory();
    let mut checked = 0usize;

    let entries = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()));

    for entry in entries {
        let path = entry.expect("cannot read directory entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yaml") {
            continue;
        }

        let scenario = Scenario::load(&path)
            .unwrap_or_else(|error| panic!("cannot load {}: {error}", path.display()));
        scenario
            .validate()
            .unwrap_or_else(|error| panic!("{} is invalid: {error}", path.display()));
        checked += 1;
    }

    assert!(checked > 0, "no scenario files were found in {}", directory.display());
}
