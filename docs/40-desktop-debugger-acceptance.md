# 40 — Desktop debugger acceptance

The issue statuses in this document apply to the local Git history. They do
not change the GitHub tracker. The [coverage inventory](39-desktop-debugger-coverage.md)
maps protocol and MCP operations to desktop routes and verification.

## Local issue dispositions

| Issues | Status | Evidence |
| --- | --- | --- |
| #139, #140, #141, #142 | Resolved locally | The inventory rejects missing routes and test classifications. Mounted palette and split-inspector tests cover validated input, independent panel state, and stale target rejection. Mock worker tests cover stalled siblings and cancellation before dispatch. |
| #143, #144, #145 | Resolved locally | The mounted CITnLO test selects a core, navigates to code and memory, edits and reads back a register and memory, exercises all watchpoint kinds, and expires a draft on reset. The owned ARM/Thumb test covers Run To, Step Over, Step Out, ELF provenance, code and data reads through desktop and MCP, and refusal when the expected bytes have changed. |
| #146 | Native fixture verified | Desktop and MCP return the same stopped scan results and signed pointer hop. Guest execution changes a scanned value, narrowing returns that address, and a scan replaced through MCP invalidates the previous desktop generation. Unmapped pointer reads return a worker error. |
| #147, #148, #152 | Resolved locally | The packaged CITnLO worker accepts equivalent desktop and MCP capture settings, releases capture on disable, refuses over-budget captures without preventing sibling inspection, and returns captured draw comparisons, textures and a selected shader-trace vertex. Desktop and MCP share a GDB listener and lease. AZAHAR refuses GDB and advanced capture in both clients. |
| #153 | Resolved locally | The mounted palette test adds, lists, enables and removes a cheat, cancels a destructive action, and browses guest storage. Forms use approved handles for host assets. CIA installation, system regeneration, and specialist peripheral operations still lack an owned native fixture in this test family. |
| #154 | In progress | Per-operation classifications, Windows mock coverage, packaged CPU smoke, and several desktop/MCP effect and refusal comparisons are mapped. Full release acceptance still needs the history and kernel explorer workflows (#149–#151) and the remaining fixture coverage in the inventory. |

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
JSON. The workflow uses the owned ARM/Thumb fixture for native CI.
