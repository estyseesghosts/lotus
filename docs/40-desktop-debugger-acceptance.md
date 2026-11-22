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
| #153 | Resolved locally | The mounted palette test adds, lists, enables and removes a cheat, cancels a destructive action, and browses guest storage. Forms use approved handles for host assets. CIA installation, system regeneration, and specialist peripheral operations still lack an owned native fixture in this test family. |
| #154 | In progress | Per-operation classifications, Windows mock coverage, packaged debugger smoke, and native desktop/MCP effect and refusal comparisons are mapped. Full release acceptance still needs the fixture coverage marked pending in the inventory. |

The [desktop native tests](../crates/desktop/tests/native_inspector.rs)
and [mounted debugger test](../crates/desktop/src/lotus_window/breakpoints.rs)
ran on Windows with the 0.1.2 software-renderer workers from
[pins.toml](../config/pins.toml). The CPU test uses
[the owned ARM/Thumb ELF and 3DSX](../fixtures/debugger/fixture.c) built by
[build-debug-fixture.py](../tools/build-debug-fixture.py). The mounted and
native GPU tests also use the local `t03.3dsx` fixture. Absolute ELF labels
are tied to the epoch and the selected ELF hash. Verified relocation and
source navigation remain tracked in #76 and #77. Memory reads use the active
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
