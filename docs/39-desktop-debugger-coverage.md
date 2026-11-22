# 39 — Desktop debugger coverage

Regenerate with `cargo run -p lotus-desktop --example export-debugger-coverage`.
The [coverage test](../crates/desktop/src/debugger_coverage.rs) checks every operation from the [shared schema](../specs/debug-v1.schema.json) and the [MCP tool router](../crates/mcp/src/server.rs).

Linked desktop routes may expose a subset of arguments. The [command palette](../crates/desktop/src/lotus_window/command_palette.rs) also builds argument forms from the shared schema and lists external-only operations with their reason. The [Run Clipboard JSON action](../crates/desktop/src/lotus_window.rs) accepts shared commands subject to normal host and worker checks. Permissions describe those checks.

Route coverage is independent of native acceptance. The host checks [command requirements](../crates/protocol/src/debug.rs) against the selected worker's negotiated features and forwards the worker's own refusal. A dash in the feature column means the shared command declares no additional feature gate. It carries no backend support guarantee.

## Shared debugger commands

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes |
| --- | --- | --- | --- | --- |
| `advance` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `asset_mount` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `audio_capture` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `audio_focus` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `backtrace` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `boot_home_menu` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `breakpoint_configure` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | control | - | - |
| `breakpoint_remove` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | control | - | - |
| `breakpoint_set` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | control | - | - |
| `breakpoints` | [Breakpoint manager](../crates/desktop/src/lotus_window/breakpoints.rs) | read | - | Settings shown when acknowledged by this host in the current guest epoch. |
| `capture_scaled` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `cheat_add` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `cheat_enable` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `cheat_remove` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `cheats` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `citrace_replay` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `citrace_start` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `citrace_status` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `citrace_stop` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `content_transform` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `continue` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `crash_report` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `deadlocks` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `disassemble` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - |
| `dsp_inspect` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `dsp_load_firmware` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `events` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `events_configure` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | event_subscriptions | - |
| `fork` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `gpu_configure` | [GPU capture form](../crates/desktop/src/lotus_window/gpu_capture.rs) | control | gpu_capture_levels when capture is set | Named presets and all capture options (#147). |
| `gpu_draw` | [inspector rows](../crates/desktop/src/inspection.rs) | read | - | - |
| `gpu_draws` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `handles` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `hio_forward` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `hio_reply` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `input_state` | [desktop input](../crates/desktop/src/input_routing.rs) | control | - | Local input devices supply state through the input router |
| `install_cia` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `lease_acquire` | external only | control | - | Lease ownership belongs to the external controller |
| `lease_release` | external only | control | - | Lease ownership belongs to the external controller |
| `load` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `load_state` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - |
| `load_symbols` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `logs` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `matrix` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `memory_map` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | read | - | - |
| `movie_play` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `movie_record` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `movie_status` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `movie_stop` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `nfc_remove` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `pause` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `perf` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `pointer_chain` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `processes` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `read_memory` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | read | - | - |
| `registers` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `reset` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `resolve_symbol` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `reverse_continue` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `reverse_step` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `rewind_configure` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | rewind_compression_queue when compression_queue is set | - |
| `rewind_status` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `run_to` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - |
| `run_until` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `save_state` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `scan` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `scan_narrow` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `scan_results` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `settings_diff` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `settings_get` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `settings_list` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `settings_set` | [inspector rows](../crates/desktop/src/inspection.rs) | control | deterministic_async for deterministic_async_operations | - |
| `shader_trace` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - |
| `state_hash` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control if dump is set | state_hash | - |
| `states` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Cached session summaries and gauges |
| `step` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `step_out` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `step_over` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `stop` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `storage_browse` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `surface` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - |
| `suspend` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `symbolize` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `system_regenerate` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |
| `thread_registers` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `threads` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `titles` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | read | - | - |
| `trace` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `write_memory` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | control | - | - |
| `write_register` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | control | - | - |
| `write_thread_register` | [palette](../crates/desktop/src/lotus_window/command_palette.rs) | control | - | - |

## MCP debugger tools

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes |
| --- | --- | --- | --- | --- |
| `lotus_capture` | external only | read | - | Raw artifacts are exposed to external clients |
| `lotus_capture_image` | external only | read | - | Inline images are an MCP response format |
| `lotus_debug` | debug bridge | per operation | - | See the shared command inventory |
| `lotus_gdb` | [GDB inspector](../crates/desktop/src/lotus_window/gdb_panel.rs) | control to start or stop | - | Shares the listener and client lease with the desktop (#152). CITnLO only. |
| `lotus_read_memory` | [Memory inspector](../crates/desktop/src/lotus_window/memory_panel.rs) | read | - | Bounded guest reads with hex and ASCII |
| `lotus_registers` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | CPU core is selectable |

## MCP host tools

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes |
| --- | --- | --- | --- | --- |
| `lotus_control` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Run, pause, reset and stop |
| `lotus_create_session` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Create or launch a card or secondary |
| `lotus_focus_session` | [workspace](../crates/desktop/src/lotus_window.rs) | presentation change | - | Select a session in the desktop |
| `lotus_input` | [desktop input](../crates/desktop/src/input_routing.rs) | control | - | Device input through the desktop router |
| `lotus_input_presets` | [settings window](../crates/desktop/src/settings_window.rs) | read or control by operation | - | Preset library and editor |
| `lotus_list_cards` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Card sidebar |
| `lotus_list_sessions` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Session sidebar |
| `lotus_load` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Asset picker |
| `lotus_pin_artifact` | external only | control | - | External artifact retention |
| `lotus_release_artifacts` | external only | control | - | External artifact retention |
| `lotus_run_until` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Only single-frame advance in the desktop |
| `lotus_session_status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Cached session summaries and gauges |
| `lotus_settings` | [settings window](../crates/desktop/src/settings_window.rs) | read | - | Scoped settings view |
| `lotus_settings_change` | [settings window](../crates/desktop/src/settings_window.rs) | control | - | Scope and application rules apply |
| `lotus_settings_schema` | [settings window](../crates/desktop/src/settings_window.rs) | read | - | Settings registry drives the form |
| `lotus_status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Desktop gauges show session status, not MCP server diagnostics |
| `lotus_validate_scenario` | external only | read | - | Scenario authoring is currently external |
| `lotus_workspace` | [workspace](../crates/desktop/src/lotus_window.rs) | read or control by operation | - | Interactive tiling shares the workspace model |
