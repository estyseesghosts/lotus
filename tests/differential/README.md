# Differential tests

Two runs, one reference and one candidate, compared frame by frame and
checkpoint by checkpoint.

Rules:

- Both runs use the same content hash, the same input schedule and the same
  deterministic initial tick.
- The comparison runs on raw worker output, never on a scaled or composited
  screenshot.
- Exact match and tolerance based match are reported separately. "Close" is
  not "equal".
- The first differing checkpoint is reported: CPU checkpoint, GPU command,
  framebuffer or service response.
- The result records the emulator revision, backend, configuration, game
  build, instrumentation settings and the host environment.

The comparison itself is implemented in
[crates/accuracy](../crates/accuracy/src/lib.rs).
