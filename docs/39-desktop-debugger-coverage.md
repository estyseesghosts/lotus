# 39 — Desktop debugger coverage

Regenerate with `cargo run -p lotus-desktop --example export-debugger-coverage`.
The [coverage test](../crates/desktop/src/debugger_coverage.rs) checks every operation from the [shared schema](../specs/debug-v1.schema.json) and the [MCP tool router](../crates/mcp/src/server.rs).

Linked desktop routes may expose a subset of arguments. The [command palette](../crates/desktop/src/lotus_window/command_palette.rs) also builds argument forms from the shared schema and lists external-only operations with their reason. The [Run Clipboard JSON action](../crates/desktop/src/lotus_window.rs) accepts shared commands subject to normal host and worker checks. Permissions describe those checks.

Route coverage is independent of native acceptance. The host checks [command requirements](../crates/protocol/src/debug.rs) against the selected worker's negotiated features and forwards the worker's own refusal. A dash in the feature column means the shared command declares no additional feature gate. It carries no backend support guarantee.

Verification links to the corresponding desktop test family. GPU and GDB exercises include shared desktop/MCP calls on packaged workers. Memory, Breakpoints, and Guest tools include mounted interaction with CITnLO. CPU includes an owned ARM/Thumb fixture. Source identity binds clang and devkitARM builds of that fixture and checks refusals of other builds. Source debugging sets line breakpoints and steps lines on clang and devkitARM builds of a C++ fixture, checked against llvm-addr2line and llvm-objdump. Variables reads locals, arguments, globals and expressions of -O1 clang and devkitARM builds of a C fixture and compares them with the values the fixture defines. Workspace and Input use mock workers. Fixture pending means the route still needs mapped desktop acceptance. This inventory records test coverage independently of the outcome of any release run.

## Shared debugger commands

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes | Verification |
| --- | --- | --- | --- | --- | --- |
| `advance` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | fixture pending |
| `asset_mount` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `audio_capture` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - | fixture pending |
| `audio_focus` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - | fixture pending |
| `backtrace` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `boot_home_menu` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `breakpoint_configure` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | control | - | - | [Breakpoints](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `breakpoint_remove` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | control | - | - | [Breakpoints](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `breakpoint_set` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | control | - | - | [Breakpoints](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `breakpoints` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | read | - | Settings shown when acknowledged by this host in the current guest epoch. | [Breakpoints](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `capture_scaled` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | fixture pending |
| `cheat_add` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `cheat_enable` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `cheat_remove` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `cheats` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `citrace_replay` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `citrace_start` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `citrace_status` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | read | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `citrace_stop` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `content_transform` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `continue` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | fixture pending |
| `crash_report` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | fixture pending |
| `deadlocks` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `disassemble` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `dsp_inspect` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | fixture pending |
| `dsp_load_firmware` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `events` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | read | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `events_configure` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | event_subscriptions | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `fork` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `gpu_configure` | [GPU capture form](../crates/desktop/src/lotus_window/gpu_capture.rs) | control | gpu_capture_levels when capture is set | Named presets and all capture options (#147). | [GPU](../crates/desktop/tests/native_inspector.rs) |
| `gpu_draw` | [inspector rows](../crates/desktop/src/inspection.rs) | read | - | - | [GPU](../crates/desktop/tests/native_inspector.rs) |
| `gpu_draws` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | [GPU](../crates/desktop/tests/native_inspector.rs) |
| `handles` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `hio_forward` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `hio_reply` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `input_state` | [desktop input](../crates/desktop/src/input_routing.rs) | control | - | Local input devices supply state through the input router | [Input](../crates/desktop/tests/input_routing.rs) |
| `inspect_expression` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Variables](../crates/desktop/tests/native_inspector.rs) |
| `inspect_variable` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [Variables](../crates/desktop/tests/native_inspector.rs) |
| `install_cia` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `lease_acquire` | external only | control | - | Lease ownership belongs to the external controller | external |
| `lease_release` | external only | control | - | Lease ownership belongs to the external controller | external |
| `load` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | fixture pending |
| `load_state` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `load_symbols` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | [Source identity](../crates/desktop/tests/native_inspector.rs) |
| `logs` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | fixture pending |
| `matrix` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `memory_map` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | read | - | - | [Memory](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `movie_play` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `movie_record` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `movie_status` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | read | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `movie_stop` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `nfc_remove` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `pause` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | fixture pending |
| `perf` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | fixture pending |
| `pointer_chain` | [Memory search](../crates/desktop/src/lotus_window/memory_search.rs) | read | - | - | [Memory search](../crates/desktop/tests/native_inspector.rs) |
| `processes` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `read_memory` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | read | - | - | [Memory](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `registers` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `reset` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | fixture pending |
| `resolve_symbol` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Source identity](../crates/desktop/tests/native_inspector.rs) |
| `reverse_continue` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | fixture pending |
| `reverse_step` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | fixture pending |
| `rewind_configure` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | rewind_compression_queue when compression_queue is set | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `rewind_status` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | read | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `run_to` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `run_until` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `save_state` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | control | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `scan` | [Memory search](../crates/desktop/src/lotus_window/memory_search.rs) | control | - | - | [Memory search](../crates/desktop/tests/native_inspector.rs) |
| `scan_narrow` | [Memory search](../crates/desktop/src/lotus_window/memory_search.rs) | control | - | - | [Memory search](../crates/desktop/tests/native_inspector.rs) |
| `scan_results` | [Memory search](../crates/desktop/src/lotus_window/memory_search.rs) | read | - | - | [Memory search](../crates/desktop/tests/native_inspector.rs) |
| `settings_diff` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | fixture pending |
| `settings_get` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | fixture pending |
| `settings_list` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - | fixture pending |
| `settings_set` | [inspector rows](../crates/desktop/src/inspection.rs) | control | deterministic_async for deterministic_async_operations | - | fixture pending |
| `shader_trace` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - | [GPU](../crates/desktop/tests/native_inspector.rs) |
| `source_breakpoint_remove` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `source_breakpoint_set` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `source_breakpoints` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `source_excerpt` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `source_step` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [Source debugging](../crates/desktop/tests/native_inspector.rs) |
| `state_hash` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control if dump is set | state_hash | - | fixture pending |
| `states` | [Snapshots inspector](../crates/desktop/src/lotus_window/snapshots_panel.rs) | read | - | - | [Snapshots](../crates/desktop/tests/native_inspector.rs) |
| `status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Cached session summaries and gauges | fixture pending |
| `step` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `step_out` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `step_over` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `stop` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments | fixture pending |
| `storage_browse` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Guest tools](../crates/desktop/src/lotus_window/command_palette.rs) |
| `surface` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - | fixture pending |
| `suspend` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | control | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `symbolize` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | [Source identity](../crates/desktop/tests/native_inspector.rs) |
| `system_regenerate` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - | fixture pending |
| `thread_registers` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `threads` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | read | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |
| `titles` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - | fixture pending |
| `trace` | [Event timeline](../crates/desktop/src/lotus_window/event_timeline.rs) | control | - | - | [Timeline](../crates/desktop/tests/native_inspector.rs) |
| `write_memory` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | control | - | - | [Memory](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `write_register` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - | [CPU](../crates/desktop/tests/native_inspector.rs) |
| `write_thread_register` | [Kernel explorer](../crates/desktop/src/lotus_window/kernel_panel.rs) | control | - | - | [Kernel](../crates/desktop/tests/native_inspector.rs) |

## MCP debugger tools

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes | Verification |
| --- | --- | --- | --- | --- | --- |
| `lotus_capture` | external only | read | - | Raw artifacts are exposed to external clients | external |
| `lotus_capture_image` | external only | read | - | Inline images are an MCP response format | external |
| `lotus_debug` | debug bridge | per operation | - | See the shared command inventory | fixture pending |
| `lotus_gdb` | [GDB inspector](../crates/desktop/src/lotus_window/gdb_panel.rs) | control to start or stop | - | Shares the listener and client lease with the desktop (#152). CITnLO only. | [GDB](../crates/desktop/tests/native_inspector.rs) |
| `lotus_read_memory` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | read | - | Bounded guest reads with hex and ASCII | [Memory](../crates/desktop/src/lotus_window/breakpoints.rs) |
| `lotus_registers` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | CPU core is selectable | [CPU](../crates/desktop/tests/native_inspector.rs) |

## MCP host tools

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes | Verification |
| --- | --- | --- | --- | --- | --- |
| `lotus_control` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Run, pause, reset and stop | fixture pending |
| `lotus_create_session` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Create or launch a card or secondary | fixture pending |
| `lotus_focus_session` | [workspace](../crates/desktop/src/lotus_window.rs) | presentation change | - | Select a session in the desktop | [Workspace](../crates/desktop/tests/responsive_controller.rs) |
| `lotus_input` | [desktop input](../crates/desktop/src/input_routing.rs) | control | - | Device input through the desktop router | [Input](../crates/desktop/tests/input_routing.rs) |
| `lotus_input_presets` | [settings window](../crates/desktop/src/settings_window.rs) | read or control by operation | - | Preset library and editor | fixture pending |
| `lotus_list_cards` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Card sidebar | fixture pending |
| `lotus_list_sessions` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Session sidebar | fixture pending |
| `lotus_load` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Asset picker | fixture pending |
| `lotus_pin_artifact` | external only | control | - | External artifact retention | external |
| `lotus_release_artifacts` | external only | control | - | External artifact retention | external |
| `lotus_run_until` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Only single-frame advance in the desktop | fixture pending |
| `lotus_session_status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Cached session summaries and gauges | fixture pending |
| `lotus_settings` | [settings window](../crates/desktop/src/settings_window.rs) | read | - | Scoped settings view | fixture pending |
| `lotus_settings_change` | [settings window](../crates/desktop/src/settings_window.rs) | control | - | Scope and application rules apply | fixture pending |
| `lotus_settings_schema` | [settings window](../crates/desktop/src/settings_window.rs) | read | - | Settings registry drives the form | fixture pending |
| `lotus_status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Desktop gauges show session status, not MCP server diagnostics | fixture pending |
| `lotus_validate_scenario` | external only | read | - | Scenario authoring is currently external | external |
| `lotus_workspace` | [workspace](../crates/desktop/src/lotus_window.rs) | read or control by operation | - | Interactive tiling shares the workspace model | [Workspace](../crates/desktop/tests/responsive_controller.rs) |
