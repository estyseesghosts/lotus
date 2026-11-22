# 40 — Desktop debugger acceptance

The issue statuses in this document apply to the local Git history. They do
not change the GitHub tracker. The [coverage inventory](39-desktop-debugger-coverage.md)
maps protocol and MCP operations to desktop routes and verification.

## Local issue dispositions

| Issues | Status | Evidence |
| --- | --- | --- |
| #139, #140, #141, #142 | Resolved locally | The inventory rejects missing routes and test classifications. Mounted palette and split-inspector tests cover validated input, independent panel state, and stale target rejection. Mock worker tests cover stalled siblings and cancellation before dispatch. |
| #143, #144, #145 | Resolved locally | The mounted CITnLO test selects a core, navigates to code and memory, edits and reads back a register and memory, exercises all watchpoint kinds, and expires a draft on reset. The owned ARM/Thumb test covers Run To, Step Over, Step Out, ELF provenance, code and data reads through desktop and MCP, and refusal when the expected bytes have changed. |
| #146 | Resolved locally | Desktop and MCP return the same stopped scan results and signed pointer hop. Guest execution changes a scanned value, narrowing returns that address, and replacing the scan through MCP or resetting the guest invalidates it. The mounted test opens a match in the hex viewer. Unmapped links return the worker's diagnostic. |
| #149 | Resolved locally | The history test reads back all rewind settings, checks the active reservation and sibling budget refusal, restores a named state and checks epoch invalidation, and reopens the same primary card to replay a recorded movie read-only. A boot profile change rejects the saved checkpoint. Reverse execution controls use the existing worker operations. |
| #150 | Resolved locally | The timeline test compares desktop and MCP subscriptions, produces SVC and guest-log records with deliberate loss, and records, exports and replays a CiTrace with source provenance and an independently inspectable sibling. The local GPU fixture produces PICA and IPC records on both guest models. |
| #151 | Resolved locally | The owned fixture verifies linked process/thread data, active-stack unwind, inspected handles and a native mutex cycle with matching MCP evidence. It checks register readback, suspension, resumption, and refusal of a wrong process or expired guest. The pinned worker exposes native unwinding only for a currently active core. Other threads show their saved context. Runtime-verified source mapping remains in #76. |
| #147, #148, #152 | Resolved locally | The packaged CITnLO worker accepts equivalent desktop and MCP capture settings, releases capture on disable, refuses over-budget captures without preventing sibling inspection, and returns captured draw comparisons, textures and a selected shader-trace vertex. Desktop and MCP share a GDB listener and lease. AZAHAR refuses GDB and advanced capture in both clients. |
| #153 | Resolved locally | The mounted palette test adds, lists, enables and removes a cheat, cancels a destructive action, and browses guest storage. Forms use approved handles for host assets. The native peripheral test under #154 covers system regeneration and specialist peripheral dispatch, including refusal of a non-CIA input. |
| #76 | Resolved locally | Clang and devkitARM GCC builds of the owned fixture bind only to the guest that loaded them. Desktop and MCP return the same refusal for the other compiler's ELF and for a rebuild with shifted layout, and the same symbolized locations, which match `llvm-nm` and `llvm-addr2line`. Two sessions resolve `thumb_tick` to their own builds. Overwriting the content and resetting drops the binding, refuses the old ELF and accepts the rebuilt one with its new content hash. `svcExitProcess` drops the binding in the same epoch. `ctr-cli debug` repeats the refusal, binding, lookup and reset checks. |
| #77 | Resolved locally | Clang and devkitARM GCC builds of the owned C++ fixture stop at source breakpoints and step into, over and out through ARM to Thumb calls, a linker veneer, a method and an inlined function, matching `llvm-addr2line -i` and `llvm-objdump --line-numbers`. Breakpoints re-arm after reset and after a rebuild with shifted layout, and stay pending for an ELF that no longer matches. A stripped ELF is refused. Excerpts are refused outside source roots or with a changed MD5. The CPU inspector toggles a breakpoint from a source row. `ctr-cli debug` repeats the breakpoint and step checks. Only CITnLO was tested. |
| #78 | Resolved locally | Clang and devkitARM GCC `-O1` builds of the owned C fixture return the values the fixture defines for register, stack and location-list locals, stack arguments, entry values resolved from call sites (GCC), an inlined frame, a Thumb frame, and callee-saved `depth` in three recursive frames restored by CFI unwinding. A global covers nested structs, an array, bit-fields, an enum, a union, a cyclic list cut at `cycle`, a pointer into unmapped memory (`invalid_pointer`), a function pointer and a 64-bit integer. Optimized-out and unavailable values carry reasons. Reads refuse a running guest. Expressions, the desktop request path, the CPU inspector rows and `ctr-cli debug` return the same values. Only CITnLO was tested. |
| #154 | In progress | Per-operation classifications, Windows mock coverage, packaged debugger smoke, and native desktop/MCP effect and refusal comparisons are mapped. The session test runs one card through the desktop and another through MCP on both pins, from boot with deterministic async operations. It compares the card and session listings; load, reset, advance and run_until; the state hash; status, performance, settings, title, crash and log reads; a scaled capture; forks; a refused and an accepted live setting; rewind, instruction steps and reverse execution; continue, pause, reload and stop. The worker starts a new epoch for each reverse operation, so the desktop withholds that reply as stale and the test compares the state on the new epoch. AZAHAR refuses run_until with the same message in both clients. The mounted Settings parity test covers `lotus_settings`, `lotus_settings_change`, `lotus_settings_schema` and the `lotus_input_presets` list against a headless server on the same host configuration; the window has no preset library editor. The peripheral and system test covers the remaining 14 inventory operations on both Windows pins. See the scope of the verified refusals below. |

The native peripheral test mounts generated camera and microphone media, rejects
an invalid NFC file, removes NFC media, checks audio focus and publishes a WAV
from a paused session. It runs a two-profile matrix and compares the exported
checkpoint registers and screen hashes. On CITnLO it also regenerates session
identity, inspects HLE DSP state, reads a 64×64 VRAM surface, compresses the owned
3DSX and delivers an HIO reply to the fixture's mode-5 request. AZAHAR returns
matching Unsupported responses through desktop and MCP for the experimental
operations. CIA install, HOME Menu boot and DSP firmware load are exercised
with inputs or state that require refusal. Successful installation, HOME Menu
boot, DSP firmware loading and valid NFC mounting remain unverified.

The [desktop native tests](../crates/desktop/tests/native_inspector.rs)
and [mounted debugger test](../crates/desktop/src/lotus_window/breakpoints.rs)
ran on Windows with the 0.1.2 software-renderer workers from
[pins.toml](../config/pins.toml). The CPU test uses
[the owned ARM/Thumb ELF and 3DSX](../fixtures/debugger/fixture.c) built by
[build-debug-fixture.py](../tools/build-debug-fixture.py). The mounted and
native GPU tests also use the local `t03.3dsx` fixture. Absolute ELF labels
are tied to the epoch and the selected ELF hash. Source navigation is
covered by #76 and #77; relocated CRO modules stay raw addresses. Memory reads use the active
guest process. The GPU resource inspector shows captured physical addresses,
which are distinct from the virtual addresses used by the memory editor.

The [Windows release workflow](../.github/workflows/windows.yml) stages a
native desktop smoke executable alongside the application build. The portable
acceptance step runs it against the workers extracted from the ZIP, then
records passed, unsupported and untested outcomes in the release acceptance
JSON. The workflow uses the owned ARM/Thumb fixture for native CI, covering
the CPU, memory search, snapshots, timeline, and kernel explorer. The AZAHAR
checks confirm the same desktop and MCP refusals for scans, rewind, CiTrace,
and thread register commands.

### devkitPro project builds

Source inspected and verified on Windows with the pinned CITnLO worker
(`a3994c1da5d2c6e00731a20c578a54a1b6d38fc7`, Release) and devkitARM GCC 16.1.0.
`devkitpro_builds_relaunch_in_the_window_host` builds a libctru fixture in two project roots
through window-host MCP, checks that a failed and a cancelled job preserve the selected ELF and
epoch, then rebuilds, compares hashes and runs to the re-armed source breakpoint.
`cli_build_jobs_are_available_between_requests` sends the same build and relaunch commands over
the CLI's JSON-lines interface against the CITnLO and AZAHAR Windows workers.
The fixture and profile are in [build and run](27-build-and-run.md).
