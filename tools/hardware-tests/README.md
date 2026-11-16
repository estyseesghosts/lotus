# Hardware test programs

**Not started.** This directory holds small devkitPro 3DS programs that run
on real hardware and produce the ground truth the emulator is compared
against.

## Rules

- Every test must state what it measures and in which time domain:
  hardware cycles, guest ticks, or host wall clock. They are never mixed.
- Every test must have a bounded run: a frame count, a cycle budget and a
  stop condition.
- A hardware result is only a result if the test proves it actually executed
  the intended path. Use a magic value or a checksum that can only be
  produced by the measured code.
- Record the measurement overhead of the instrumentation itself.

## Layout (planned)

```
tools/hardware-tests/
├── Makefile              # devkitPro 3DS build
├── common/               # timing helpers, result reporting
├── gpu/                  # PICA200 counters, draw cost
├── memory/               # bus and cache behaviour
└── results/              # captures, never committed as binary
```

Captures are user assets: they stay out of version control.
