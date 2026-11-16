# Test tree

| Directory     | Purpose                                                       |
| ------------- | ------------------------------------------------------------- |
| `smoke/`      | Fast checks that the workspace, the CLI and the worker stub work. |
| `integration/`| Multi component checks: supervisor, worker, IPC and the UI.   |
| `differential/`| Two runs, one reference and one candidate, compared.          |
| `fixtures/`   | Test content: ROMs, homebrew programs, saves. Never committed.|

Scenarios live in `scenarios/`. They are YAML files that the runner loads and
validates before anything is executed; see
[crates/runner](../crates/runner/src/lib.rs) and
[specs/scenario-example.yaml](../specs/scenario-example.yaml).

## Rules for every test

- A test counts only when it is reproducible: fixed input, known source and
  build hashes, isolated storage, bounded stop condition.
- Guest behaviour, host performance and estimated hardware timing are
  reported separately and never mixed.
- Fixtures that contain game data, firmware or saves stay out of version
  control. They are user assets.
