# Smoke tests

Fast, self contained checks that catch a broken environment before a long
run starts.

Planned checks:

- `cargo build --workspace` succeeds with the pinned toolchain.
- `cargo test --workspace` passes.
- `ctr-cli version` and `ctr-cli pins` report the pinned revisions.
- The worker stub prints a handshake that matches the Rust side of the ABI.
- `scripts/verify-pins.ps1` reports every pin as matching.

Run them with:

```powershell
scripts/smoke.ps1
```
