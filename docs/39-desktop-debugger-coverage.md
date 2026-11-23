# 39 — Desktop debugger coverage

Regenerate with `cargo run -p lotus-desktop --example export-debugger-coverage`.
The [coverage test](../crates/desktop/src/debugger_coverage.rs) checks every operation from the [shared schema](../specs/debug-v1.schema.json) and the [MCP tool router](../crates/mcp/src/server.rs).

Linked desktop routes may expose a subset of arguments. The [command palette](../crates/desktop/src/lotus_window/command_palette.rs) also builds argument forms from the shared schema and lists external-only operations with their reason. The [Run Clipboard JSON action](../crates/desktop/src/lotus_window.rs) accepts shared commands subject to normal host and worker checks. Permissions describe those checks.

Route coverage is independent of native acceptance. The host checks [command requirements](../crates/protocol/src/debug.rs) against the selected worker's negotiated features and forwards the worker's own refusal. A dash in the feature column means the shared command declares no additional feature gate. It carries no backend support guarantee.

Verification links to the corresponding desktop test family. GPU and GDB exercises include shared desktop/MCP calls on packaged workers. Memory, Breakpoints, and Guest tools include mounted interaction with CITnLO. CPU includes an owned ARM/Thumb fixture. Source identity binds clang and devkitARM builds of that fixture and checks refusals of other builds. Source debugging sets line breakpoints and steps lines on clang and devkitARM builds of a C++ fixture, checked against llvm-addr2line and llvm-objdump. Variables reads locals, arguments, globals and expressions of -O1 clang and devkitARM builds of a C fixture and compares them with the values the fixture defines. Builds exercises fixture rebuilds and two-session isolation through the window host and compares desktop and MCP status. Workspace and Input use mock workers. Fixture pending means the route still needs mapped desktop acceptance. This inventory records test coverage independently of the outcome of any release run.

## Shared debugger commands

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes | Verification |
| --- | --- | --- | --- | --- | --- |
| `advance` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | [Session](../crates/desktop/tests/native_inspector.rs) |
| `asset_mount` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals (mount and refusal)](../crates/desktop/tests/native_inspector.rs) |
| `audio_capture` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `audio_focus` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `backtrace` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `boot_home_menu` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals (refusal)](../crates/desktop/tests/native_inspector.rs) |
| `breakpoint_configure` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | control | - | - | [Breakpoints](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `breakpoint_remove` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | control | - | - | [Breakpoints](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `breakpoint_set` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | control | - | - | [Breakpoints](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `breakpoints` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | read | - | Settings shown when acknowledged by this host in the current guest epoch. | [Breakpoints](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `build` | [Builds inspector](../crates/desktop/src/lotus_window/builds_panel.rs) | control | - | Trusted host build profiles | [Builds](../crates/desktop/tests/native_inspector.rs) |
| `build_cancel` | [Builds inspector](../crates/desktop/src/lotus_window/builds_panel.rs) | control | - | Trusted host build profiles | [Builds](../crates/desktop/tests/native_inspector.rs) |
| `build_status` | [Builds inspector](../crates/desktop/src/lotus_window/builds_panel.rs) | read | - | Trusted host build profiles | [Builds](../crates/desktop/tests/native_inspector.rs) |
| `capture_scaled` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `cheat_add` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `cheat_enable` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `cheat_remove` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `cheats` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `citrace_replay` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `citrace_start` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `citrace_status` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | read | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `citrace_stop` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `compare_builds` | [Builds inspector](../crates/desktop/src/lotus_window/builds_panel.rs) | read | - | Trusted host build profiles | [Builds](../crates/desktop/tests/native_inspector.rs) |
| `content_transform` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `continue` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | [Session](../crates/desktop/tests/native_inspector.rs) |
| `crash_report` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `deadlocks` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `disassemble` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `dsp_inspect` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `dsp_load_firmware` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals (refusal)](../crates/desktop/tests/native_inspector.rs) |
| `events` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | read | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `events_configure` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | event_subscriptions | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `fork` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `gpu_configure` | [GPU capture form](../crates/desktop/src/lotus_window/gpu_capture.rs) | control | gpu_capture_levels when capture is set | Named presets and all capture options (#147). | [GPU](../crates/desktop/tests/native_inspector.rs) |
| `gpu_draw` | [inspector rows](../crates/desktop/src/inspection.rs) | read | - | - | [GPU](../crates/desktop/tests/native_inspector.rs) |
| `gpu_draws` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | [GPU](../crates/desktop/tests/native_inspector.rs) |
| `handles` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `hio_forward` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `hio_reply` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `input_state` | [desktop input](../crates/desktop/src/input_routing.rs) | control | - | Local input devices supply state through the input router | [Input](../crates/desktop/tests/input_routing.rs) |
| `inspect_expression` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Variables](../crates/desktop/tests/native_inspector.rs) |
| `inspect_variable` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [Variables](../crates/desktop/tests/native_inspector.rs) |
| `install_cia` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals (refusal)](../crates/desktop/tests/native_inspector.rs) |
| `lease_acquire` | external only | control | - | Lease ownership belongs to the external controller | external |
| `lease_release` | external only | control | - | Lease ownership belongs to the external controller | external |
| `load` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | [Session](../crates/desktop/tests/native_inspector.rs) |
| `load_state` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `load_symbols` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Source identity](../crates/desktop/tests/native_inspector.rs) |
| `logs` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `matrix` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `memory_map` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | read | - | - | [Memory](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `movie_play` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `movie_record` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `movie_status` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | read | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `movie_stop` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `nfc_remove` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `pause` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | [Session](../crates/desktop/tests/native_inspector.rs) |
| `perf` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `pointer_chain` | [Memory search](../crates/desktop/src/lotus_window/memory_search.rs) | read | - | - | [Memory search](../crates/desktop/tests/native_inspector.rs) |
| `processes` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `read_memory` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | read | - | - | [Memory](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `registers` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `relaunch_build` | [Builds inspector](../crates/desktop/src/lotus_window/builds_panel.rs) | control | - | Trusted host build profiles | [Builds](../crates/desktop/tests/native_inspector.rs) |
| `reset` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | [Session](../crates/desktop/tests/native_inspector.rs) |
| `resolve_symbol` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Source identity](../crates/desktop/tests/native_inspector.rs) |
| `reverse_continue` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `reverse_step` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `rewind_configure` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | rewind_compression_queue when compression_queue is set | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `rewind_status` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | read | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `run_to` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `run_until` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `save_state` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `scan` | [Memory search](../crates/desktop/src/lotus_window/memory_search.rs) | control | - | - | [Memory search](../crates/desktop/tests/native_inspector.rs) |
| `scan_narrow` | [Memory search](../crates/desktop/src/lotus_window/memory_search.rs) | control | - | - | [Memory search](../crates/desktop/tests/native_inspector.rs) |
| `scan_results` | [Memory search](../crates/desktop/src/lotus_window/memory_search.rs) | read | - | - | [Memory search](../crates/desktop/tests/native_inspector.rs) |
| `settings_diff` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `settings_get` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `settings_list` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `settings_set` | [inspector rows](../crates/desktop/src/inspection.rs) | control | deterministic_async for deterministic_async_operations | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `shader_trace` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - | [GPU](../crates/desktop/tests/native_inspector.rs) |
| `source_breakpoint_remove` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `source_breakpoint_set` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `source_breakpoints` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `source_excerpt` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `source_step` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `state_hash` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control if dump is set | state_hash | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `states` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | read | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Cached session summaries and gauges | [Session](../crates/desktop/tests/native_inspector.rs) |
| `step` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `step_out` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `step_over` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `stop` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | [Session](../crates/desktop/tests/native_inspector.rs) |
| `storage_browse` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `surface` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `suspend` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | control | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `symbolize` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Source identity](../crates/desktop/tests/native_inspector.rs) |
| `system_regenerate` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Peripherals](../crates/desktop/tests/native_inspector.rs) |
| `thread_registers` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `threads` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `titles` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Session](../crates/desktop/tests/native_inspector.rs) |
| `trace` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `write_memory` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | control | - | - | [Memory](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `write_register` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `write_thread_register` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | control | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |

## MCP debugger tools

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes | Verification |
| --- | --- | --- | --- | --- | --- |
| `lotus_capture` | external only | read | - | Raw artifacts are exposed to external clients | external |
| `lotus_capture_image` | external only | read | - | Inline images are an MCP response format | external |
| `lotus_debug` | debug bridge | per operation | - | See the shared command inventory | [Session](../crates/desktop/tests/native_inspector.rs) |
| `lotus_gdb` | [GDB inspector](../crates/desktop/src/lotus_window/gdb_panel.rs) | control to start or stop | - | Shares the listener and client lease with the desktop (#152). CITnLO only. | [GDB](../crates/desktop/tests/native_inspector.rs) |
| `lotus_read_memory` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | read | - | Bounded guest reads with hex and ASCII | [Memory](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `lotus_registers` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | CPU core is selectable | [CPU](../crates/desktop/tests/native_inspector.rs) |

## MCP host tools

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes | Verification |
| --- | --- | --- | --- | --- | --- |
| `lotus_control` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Run, pause, reset and stop | [Session](../crates/desktop/tests/native_inspector.rs) |
| `lotus_create_session` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Create or launch a card or secondary | [Session](../crates/desktop/tests/native_inspector.rs) |
| `lotus_focus_session` | [workspace](../crates/desktop/src/lotus_window.rs) | presentation change | - | Select a session in the desktop | [Workspace](../crates/desktop/tests/responsive_controller.rs) |
| `lotus_input` | [desktop input](../crates/desktop/src/input_routing.rs) | control | - | Device input through the desktop router | [Input](../crates/desktop/tests/input_routing.rs) |
| `lotus_input_presets` | [settings window](../crates/desktop/src/settings_window.rs) | read or control by operation | - | The Controls preset choice lists builtin and saved presets. Import, export, duplicate, rename and delete are MCP and ctr-cli only. | [Settings](../crates/desktop/src/settings_window/shared.rs) |
| `lotus_investigation` | [investigations window](../crates/desktop/src/investigation_window.rs) | read or control by operation | - | Windows editor covers notes, session references and redacted archives. CLI and MCP expose the full record and screenshot selection. | [Investigations](../crates/desktop/../../tests/test_shared_desktop_mcp.py) |
| `lotus_investigation_restore_breakpoint` | unavailable | control | - | Restore requires a verified ELF and content and a paused target. Desktop action pending. | [Investigations](../crates/desktop/../../tests/test_shared_desktop_mcp.py) |
| `lotus_list_cards` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Card sidebar | [Session](../crates/desktop/tests/native_inspector.rs) |
| `lotus_list_sessions` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Session sidebar | [Session](../crates/desktop/tests/native_inspector.rs) |
| `lotus_load` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Asset picker | [Session](../crates/desktop/tests/native_inspector.rs) |
| `lotus_pin_artifact` | external only | control | - | External artifact retention | external |
| `lotus_record_test` | external only | read or control by operation | input_boundary | Record live input and checkpoints, then export a scenario. CLI `record-test` and MCP only; desktop workflow pending. | external |
| `lotus_regression_history` | [regression history](../crates/desktop/src/regression_window.rs) | read | - | List, inspect and compare the configured host's retained suites. | [Regression history](../crates/desktop/../../tests/test_regression_history.py) |
| `lotus_release_artifacts` | external only | control | - | External artifact retention | external |
| `lotus_run_until` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Only single-frame advance in the desktop | [Session](../crates/desktop/tests/native_inspector.rs) |
| `lotus_session_status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Cached session summaries and gauges | [Session](../crates/desktop/tests/native_inspector.rs) |
| `lotus_settings` | [settings window](../crates/desktop/src/settings_window.rs) | read | - | Scoped settings view | [Settings](../crates/desktop/src/settings_window/shared.rs) |
| `lotus_settings_change` | [settings window](../crates/desktop/src/settings_window.rs) | control | - | Scope and application rules apply | [Settings](../crates/desktop/src/settings_window/shared.rs) |
| `lotus_settings_schema` | [settings window](../crates/desktop/src/settings_window.rs) | read | - | Settings registry drives the form | [Settings](../crates/desktop/src/settings_window/shared.rs) |
| `lotus_status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Desktop gauges show session status, not MCP server diagnostics | [Session](../crates/desktop/tests/native_inspector.rs) |
| `lotus_validate_scenario` | external only | read | - | Scenario authoring is currently external | external |
| `lotus_workspace` | [workspace](../crates/desktop/src/lotus_window.rs) | read or control by operation | - | Interactive tiling shares the workspace model | [Workspace](../crates/desktop/tests/responsive_controller.rs) |
