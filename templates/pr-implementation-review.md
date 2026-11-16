# Implementation PR review

**PR / branch / ticket:**  
**Azahar upstream SHA / integration SHA / experimental SHA:**  
**Rust/GPUI SHA / protocol version:**  
**Files touched and why:**

## Behavior

- Before:
- After:
- Which emulator APIs/hooks changed?
- Why does this belong in `integration` vs `experimental`?
- Any hidden Qt/Libretro/state dependency removed?

## Correctness

- Baseline binary+script:
- Input ownership and stop condition:
- Expected/actual output hash and captured raw state:
- Concurrent worker and stop/restart tests:
- Negative/fault injection tests:
- Instrumentation-on/off comparison:

## Performance

- Host environment, measurement units:
- Host overhead before/after (not hardware timing):
- Guest-tick changes:
- Hardware test (if any):

## Risk and release

- Backward protocol compatibility:
- Save-state compatibility:
- Source/third-party licensing notes:
- Privacy/security changes:
- Feature flag and rollback procedure:
- Follow-up work / known failure:
