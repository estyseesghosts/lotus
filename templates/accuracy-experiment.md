# Accuracy experiment template

**Experiment ID:** `<id>`  
**Status:** proposed | compiled | emulator run | hardware run | independently replicated  
**Owner / date:**  
**Hardware model / FW / CFW / clocks:**  
**Build and toolchain hashes:**  
**Azahar upstream/integration/experimental SHA:**

## Question and hypothesis

- What exactly is unknown?
- What does the model predict in physically measurable terms?
- What observation would refute that prediction?
- What competing explanations would produce the same result?

## Isolation

- Initial machine state, cache/VRAM contents, warmup method:
- Independent variable and sweep range:
- Controls held constant:
- Timer source, read overhead, wrap and units:
- Negative test / control test:

## Procedure

1. Build test with pinned sources and record SHA-256.
2. Execute warmup separately.
3. Collect raw values with no hot-loop logs.
4. Repeat in randomized order / separate sessions.
5. Run exactly same guest fixture across emulator baselines.

## Observations

| Run | Device/build | Value | Units | Valid? | Notes |
|---|---|---:|---|---|---|
| | | | | | |

## Analysis

- Median, spread, p95/p99, error bars:
- Observed emulator mismatch:
- Unexpected null/negative result:
- Confounders and limitations:
- Held-out test results:

## Conclusion

**Evidence class:** measured | documented | inferred | approximated | unknown  
**Claim supported (narrowly phrased):**  
**Open questions / next discriminating test:**

## Artifact references

- Test binary and source SHA:
- Raw CSV/log:
- Captures, hashes:
- Report checksum:
