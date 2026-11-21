# 39 — Desktop debugger coverage

Regenerate with `cargo run -p lotus-desktop --example export-debugger-coverage`.
The [coverage test](../crates/desktop/src/debugger_coverage.rs) checks every operation from the [shared schema](../specs/debug-v1.schema.json) and the [MCP tool router](../crates/mcp/src/server.rs).

Linked desktop routes are available today and may expose a subset of arguments. Palette entries need a structured form in #141. The existing [Run Clipboard JSON action](../crates/desktop/src/lotus_window.rs) accepts shared commands subject to normal host and worker checks. Permissions describe those checks.

Route coverage is independent of native acceptance. The host checks [command requirements](../crates/protocol/src/debug.rs) against the selected worker's negotiated features and forwards the worker's own refusal. A dash in the feature column means the shared command declares no additional feature gate. It carries no backend support guarantee.

## Shared debugger commands

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes |
| --- | --- | --- | --- | --- |
| `advance` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `asset_mount` | palette | control | - | Planned in #141 |
| `audio_capture` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `audio_focus` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `backtrace` | palette | read | - | Planned in #141 |
| `boot_home_menu` | palette | control | - | Planned in #141 |
| `breakpoint_configure` | palette | control | - | Planned in #141 |
| `breakpoint_remove` | palette | control | - | Planned in #141 |
| `breakpoint_set` | palette | control | - | Planned in #141 |
| `breakpoints` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `capture_scaled` | palette | read | - | Planned in #141 |
| `cheat_add` | palette | control | - | Planned in #141 |
| `cheat_enable` | palette | control | - | Planned in #141 |
| `cheat_remove` | palette | control | - | Planned in #141 |
| `cheats` | palette | read | - | Planned in #141 |
| `citrace_replay` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `citrace_start` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `citrace_status` | palette | read | - | Planned in #141 |
| `citrace_stop` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `content_transform` | palette | control | - | Planned in #141 |
| `continue` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `crash_report` | palette | read | - | Planned in #141 |
| `deadlocks` | palette | read | - | Planned in #141 |
| `disassemble` | [CPU inspector](../crates/desktop/src/desktop_controller.rs) | read | - | - |
| `dsp_inspect` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `dsp_load_firmware` | palette | control | - | Planned in #141 |
| `events` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `events_configure` | palette | control | event_subscriptions | Planned in #141 |
| `fork` | palette | control | - | Planned in #141 |
| `gpu_configure` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | gpu_capture_levels when capture is set | - |
| `gpu_draw` | [inspector rows](../crates/desktop/src/inspection.rs) | read | - | - |
| `gpu_draws` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `handles` | palette | read | - | Planned in #141 |
| `hio_forward` | palette | control | - | Planned in #141 |
| `hio_reply` | palette | control | - | Planned in #141 |
| `input_state` | [desktop input](../crates/desktop/src/input_routing.rs) | control | - | Local input devices supply state through the input router |
| `install_cia` | palette | control | - | Planned in #141 |
| `lease_acquire` | external only | control | - | Lease ownership belongs to the external controller |
| `lease_release` | external only | control | - | Lease ownership belongs to the external controller |
| `load` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `load_state` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - |
| `load_symbols` | palette | control | - | Planned in #141 |
| `logs` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `matrix` | palette | control | - | Planned in #141 |
| `memory_map` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `movie_play` | palette | control | - | Planned in #141 |
| `movie_record` | palette | control | - | Planned in #141 |
| `movie_status` | palette | read | - | Planned in #141 |
| `movie_stop` | palette | control | - | Planned in #141 |
| `nfc_remove` | palette | control | - | Planned in #141 |
| `pause` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `perf` | palette | read | - | Planned in #141 |
| `pointer_chain` | palette | read | - | Planned in #141 |
| `processes` | palette | read | - | Planned in #141 |
| `read_memory` | palette | read | - | Planned in #141 |
| `registers` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `reset` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `resolve_symbol` | palette | read | - | Planned in #141 |
| `reverse_continue` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `reverse_step` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `rewind_configure` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | rewind_compression_queue when compression_queue is set | - |
| `rewind_status` | palette | read | - | Planned in #141 |
| `run_to` | palette | control | - | Planned in #141 |
| `run_until` | palette | control | - | Planned in #141 |
| `save_state` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `scan` | palette | control | - | Planned in #141 |
| `scan_narrow` | palette | control | - | Planned in #141 |
| `scan_results` | palette | read | - | Planned in #141 |
| `settings_diff` | palette | read | - | Planned in #141 |
| `settings_get` | palette | read | - | Planned in #141 |
| `settings_list` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `settings_set` | [inspector rows](../crates/desktop/src/inspection.rs) | control | deterministic_async for deterministic_async_operations | - |
| `shader_trace` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - |
| `state_hash` | palette | control if dump is set | state_hash | Planned in #141 |
| `states` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `status` | [session status](../crates/desktop/src/desktop_controller.rs) | read | - | Cached session summaries and gauges |
| `step` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `step_out` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `step_over` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `stop` | [session controls](../crates/desktop/src/lotus_window.rs) | control | - | Current desktop controls expose a subset of arguments |
| `storage_browse` | palette | read | - | Planned in #141 |
| `surface` | [inspector rows](../crates/desktop/src/inspection.rs) | control | - | - |
| `suspend` | palette | control | - | Planned in #141 |
| `symbolize` | palette | read | - | Planned in #141 |
| `system_regenerate` | palette | control | - | Planned in #141 |
| `thread_registers` | palette | read | - | Planned in #141 |
| `threads` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | - |
| `titles` | palette | read | - | Planned in #141 |
| `trace` | [inspector actions](../crates/desktop/src/lotus_window.rs) | control | - | - |
| `write_memory` | palette | control | - | Planned in #141 |
| `write_register` | palette | control | - | Planned in #141 |
| `write_thread_register` | palette | control | - | Planned in #141 |

## MCP debugger tools

| Operation | Desktop route | Permission | Extra worker feature | Coverage notes |
| --- | --- | --- | --- | --- |
| `lotus_capture` | external only | read | - | Raw artifacts are exposed to external clients |
| `lotus_capture_image` | external only | read | - | Inline images are an MCP response format |
| `lotus_debug` | debug bridge | per operation | - | See the shared command inventory |
| `lotus_gdb` | unavailable | control to start or stop | - | Dedicated lifecycle planned in #152 |
| `lotus_read_memory` | palette | read | - | Dedicated viewer planned in #145 |
| `lotus_registers` | [inspector polling](../crates/desktop/src/inspection.rs) | read | - | CPU inspector currently reads core 0 |

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
