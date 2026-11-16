# Native Rust emulation backend (future)

**Not started.** This directory is the future home of an independent Rust
emulation backend.

## Rules

- It must be usable behind the same worker interface as the Azahar worker.
  A backend that only the desktop application can talk to is not a backend.
- It starts as a *narrow* component, not a whole system. The first candidate
  is the PICA200 command decoder, because it has an existing execution
  reference in Azahar and an extensive public documentation base.
- Every model it implements needs three forms of evidence: documentation, a
  physical hardware measurement, and a differential test against the
  reference implementation.

## Layout (planned)

```
workers/native/
├── Cargo.toml
├── crates/
│   ├── core/        # scheduler, memory bus, timing model
│   ├── arm11/       # interpreter and cache model
│   ├── pica200/     # command decoder, rasterizer, timing model
│   └── ...
└── tests/
```

Nothing here is compiled today. See
[docs/11-accuracy-research.md](../../docs/11-accuracy-research.md) for the
method and [docs/08-gpu-and-trace.md](../../docs/08-gpu-and-trace.md) for the
instrumentation it would have to emit.
