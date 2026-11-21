# 37 — User guide

How to install, run, configure and drive LOTUS as a user: the window, the MCP
server and the CLI. Design detail and evidence live in the linked documents;
this page tells you what to do. Terms are in the [glossary](glossary.md).
Building from source is [27](27-build-and-run.md).

LOTUS runs 3DS homebrew (`.3dsx`) in isolated emulator processes ("workers")
and lets you, an agent over MCP, or a script drive and debug them. The workers
are modified Azahar. LOTUS makes no hardware-accuracy claim.

## 1. Requirements

- Windows 11 x64 with Direct3D 11. Other platforms are untested.
- Your own `.3dsx` homebrew. No ROMs, firmware, keys or DSP dump ship with
  LOTUS, and commercial content is not the target.
- Nothing else for the portable release: no installer, admin rights, Rust,
  compiler or Python.

## 2. Install

1. Download `LOTUS-v<version>-windows-x64.zip` and `SHA256SUMS.txt` from the
   [GitHub releases](https://github.com/estyseesghosts/lotus/releases).
2. Check the hash: `Get-FileHash LOTUS-v<version>-windows-x64.zip` must match
   the line in `SHA256SUMS.txt`.
3. Extract the whole ZIP to a writable folder. Paths with spaces are fine.
   Binaries are unsigned, so SmartScreen may warn.

What is in the folder:

| Path | Contents |
| --- | --- |
| `LOTUS.cmd` | starts the window with the CITnLO worker (debugger, tracing, rewind) |
| `LOTUS-AZAHAR.cmd` | same, with the AZAHAR worker (upstream-tracking, minimal hooks) |
| `launch.ps1` | the launcher both `.cmd` files call; options below |
| `bin/` | `lotus-desktop.exe`, `lotus-mcp.exe`, `ctr-cli.exe` |
| `runtimes/citnlo/`, `runtimes/azahar/` | the two `ctr-worker.exe` builds |
| `data/` | created on first run: everything you produce (default data directory; see [Data directory](#data-directory)) |
| `licenses/`, `SOURCE.md`, `release-manifest.json` | licences, exact source and build revisions |

Which worker: CITnLO has every debugger feature; AZAHAR lacks event
subscriptions, GPU capture levels and `state_hash` (feature matrix in
[35](35-release-notes.md)). Use AZAHAR to compare against upstream behaviour.

**Upgrading**: extract the new release to a new folder and copy `data/` across,
or keep data outside the release folder (below) so there is nothing to copy.
**Moving**: move the whole folder; the launcher rewrites machine-specific paths
on every start.

### Launcher options

`launch.ps1` (`packaging/windows/launch.ps1`) writes
`<data>/<backend>/host.json` on every start, then runs one of the programs.
Pass options to either `.cmd`, e.g. `LOTUS.cmd -Mode WindowedMCP game.3dsx`.

| Option | Values | Effect |
| --- | --- | --- |
| `-Backend` | `CITnLO` (default), `AZAHAR` | which worker; the `.cmd` files set it |
| `-Mode` | `Desktop` (default), `MCP`, `WindowedMCP` | window; headless MCP on stdio; window that also serves MCP on stdio |
| first argument | path to a `.3dsx` | approves it as asset `homebrew` |
| `-DataDir` | path | use this data directory for this start only |
| `-SetDataDir` | path | save this data directory in `%APPDATA%\LOTUS\lotus.json` and exit |
| `-ConfigureOnly` | switch | write `host.json` and stop |

Dragging a `.3dsx` onto a `.cmd` file is the same as passing it. The launcher
always sets `allow_control: true`. It rewrites `host.json` each start, so
**edit a copy** if you want custom settings and start the programs directly
(section 5).

### Data directory

`<data>` holds `host.json`, sessions (cards, saves, logs, settings) and
`desktop.json`. The launcher picks the first of:

1. `-DataDir <path>`
2. `data_dir` in `lotus.json` beside `launch.ps1`
3. `data_dir` in `%APPDATA%\LOTUS\lotus.json` (what `-SetDataDir` writes)
4. `data\` beside `launch.ps1` (default)

`lotus.json` is `{"data_dir": "D:/LOTUS data"}`. A relative path resolves
against the folder of the `lotus.json` that names it; `%VAR%` references are
expanded. The directory is created if missing. The `%APPDATA%` file is per
Windows user and outlives release folders, so `LOTUS.cmd -SetDataDir
"D:\LOTUS data"` once makes every later release use that directory. Existing
data is not moved; copy the old `data\` contents there yourself.

### Linux AppImage

`chmod +x LOTUS-v<version>-linux-x86_64.AppImage`, then run it. Its launcher
(`packaging/linux/AppRun`) does what `launch.ps1` does, with these options:

| Option | Values | Effect |
| --- | --- | --- |
| `--backend` | `CITnLO` (default), `AZAHAR` | which worker |
| `--mode` | `Desktop` (default), `MCP`, `WindowedMCP` | as `-Mode` above |
| last argument | path to a `.3dsx` | approves it as asset `homebrew` |
| `--data-dir` | path | data directory for this start |
| `--configure-only` | | write `host.json`, print its path and stop |
| `--import-preferences` | file | copy an old `desktop.json` (e.g. `~/.config/lotus/desktop.json`) into the data directory; refused if one exists |
| `cli ARGS...` | | run `ctr-cli` |

The data directory is the first of `--data-dir`, `$LOTUS_DATA_DIR` and
`~/.local/lotus`; relative paths resolve against the current directory.
Nothing is written inside the AppImage. `host.json` is rewritten on every
start so its worker path follows the AppImage's current mount; moving or
replacing the AppImage keeps all data. `LOTUS_DESKTOP_PREFERENCES`, if set,
still wins over `<data>/desktop.json`.

## 3. Using the window

Start `LOTUS.cmd`. The window has three parts: the session sidebar on the
left, the screens in the centre, and the inspector rail on the right.

### Sessions and cards

Every running console is a **session** with its own worker process. There
are two kinds:

- A **primary** session mounts a **card**: a persistent SD/NAND/sysdata folder
  at `<data>/<backend>/sessions/cards/<card>/`. Saves and installed data written
  by the guest stay there across restarts. One primary per card at a time.
  Card names are 1–32 characters of `a-z 0-9 - _`; a card is created on first
  use.
- A **secondary** session is disposable. It starts empty, or as a copy of a
  card at one guest instant, and its folder is deleted when it stops. Any
  number can copy one card, up to the session limit (4 by default).

In the sidebar, cards are top-level rows with their primary (square dot) and
secondaries (round dots) nested below; the dot colour is the state. Use `+`
to start a session (hovering a card's `+` copies that card), **Open file…**
or Ctrl+O to pick a `.3dsx`, and `×` to stop. A file picked here is approved
as `local/<file>` for that run only. The footer shows the MCP link, slots in
use, artifact storage and the Settings gear.

Content loads **paused**. Press Space or Run to start it.

### Controls

| Action | Keys / control |
| --- | --- |
| Run / pause | Space, or the toolbar button |
| Step one frame | `.`, or Step |
| A / B | Z / X |
| X / Y | A / S |
| L / R | Q / W |
| D-pad | arrow keys |
| Start / Select | Enter / Backspace |
| Touch screen | click or drag on the bottom screen |
| Open file | Ctrl+O |
| Toggle sidebar / inspector | Ctrl+B / Ctrl+I |
| Inspector views | Ctrl+1–7; Ctrl+[ and Ctrl+] cycle |
| Move focus / close menus | Tab, Shift+Tab / Esc |

Guest keys only reach the console while the screen has keyboard focus, and
they go to the selected session. The toolbar also has Reset, Load, Stop, zoom
and screen layout; on a narrow window they move into the `⋯` menu.

### Game controllers

Connected controllers appear in the sidebar's **CONTROLLERS** list with their
name, connection ID (`pad-N`) and session. A controller drives nothing until
you pair it:

- **Assign** pairs it with the selected session; **Replace** does so when that
  session already has one; **Unassign** frees it.
- **Identify** marks the next controller you press a button on, to tell two
  identical pads apart.
- One controller per session, at most two pairs.
- A reconnected controller returns to its session only when that is
  unambiguous; otherwise the list says "NAME disconnected; was on SESSION" and
  waits for you.

Default layout (Azahar's XInput mapping): east = A, south = B, north = X,
west = Y, D-pad, shoulders = L/R, triggers = ZL/ZR, Start, Back = Select,
Guide = Home, left stick = Circle Pad, right stick = C-stick. The builtin
`xbox-labels` preset follows the printed Xbox letters instead (south = A).
Controller support is tested with simulated devices only; real-controller
testing is pending. Windows Gaming Input delivers events only
while a LOTUS window has focus. Details: [30](30-sessions-and-interface.md#controller-pairing-62).

### Viewing two sessions

The centre shows one session, or two tiled side by side or stacked
(the workspace). Selecting a session in the sidebar shows it unless two are
tiled; selection never breaks a split. An MCP client can arrange the same
workspace. Stopping a tiled session collapses the split to the other one.
Default split axis and ratio are in `desktop.json`. Details:
[30](30-sessions-and-interface.md#workspace-67).

### Inspector

The right rail opens the inspector for the selected session: CPU,
breakpoints, threads, memory, GPU, logs, snapshots, and under **More** IPC,
timeline, audio, settings and result. Each view has its own commands;
destructive ones (Back, reverse stop, replay) are marked. The view's `⋯` menu
has **Open JSONL trace…** (event trace up to 64 MiB), **Run clipboard JSON**
(runs a debug command copied as JSON, same format as section 7) and the key
list. Several views need CITnLO.

### Gauges

Per session: FPS with a sparkline, speed (FPS ÷ 59.8312), frame and epoch,
audio sample rate and drops (output is muted), worker memory, PID and uptime,
and who last acted (`you`, `agent`, `host`). These are host measurements, not
guest timing. On a small window they collapse into pills; hover for detail.

### Settings window

The gear in the sidebar footer or Ctrl+, opens Settings; a card's gear opens
it on that card. There is one window: opening it again focuses it. Pick
**Global** or a card on the left, then a group. Each row shows where its value
comes from; **Reset to Global** (on a card) or **Reset to default** removes the value saved
at that level. Live settings update affected mounted primary sessions.
Boot, renderer and CFG changes show **Restart required**. Stop and start
the card to apply them. Input layouts and window preferences apply at once.
Host settings and `managed`
values are read-only, and so is everything but window preferences when the host
does not allow control. Text, list and binding values cannot be edited here
yet (#54); use the CLI or MCP. Esc closes it.

Pane widths and open states are saved as you drag. Discord Rich Presence
(Settings → Discord) is off by default; when on, it shows `Playing NAME` or
`Debugging ...`, and **Hide game titles** removes file names. Only the
sanitised file name is sent, over Discord's local pipe.

## 4. Settings

There are three kinds of settings, kept in different places.

### Host configuration (`host.json`)

Read once at start by `lotus-desktop`, `lotus-mcp` and `ctr-cli`. Unknown keys
are an error. Paths must exist except `asset_directories`.

| Key | Default | Meaning |
| --- | --- | --- |
| `worker` | required | path to `ctr-worker.exe` |
| `storage_root` | required | where cards, scratch roots, settings and artifacts go |
| `assets` | `{}` | handle → `.3dsx` path; only approved handles can be loaded |
| `asset_directories` | `{}` | handle → folder; each `.3dsx` directly inside is `<handle>/<file>`, re-listed on demand, so rebuilt files load without a restart |
| `allow_control` | `false` | permit anything that changes state: creating, running, input, settings changes |
| `max_sessions` | `4` | 1–16 concurrent sessions; 4 is the tested load |
| `prefer_vulkan` | `true` | new sessions use Vulkan, falling back to software if the worker refuses; `false` keeps software |
| `profile` | `{}` | worker settings forced on every session; read-only everywhere else |
| `workers` | `{}` | name → other `ctr-worker.exe`, used by the debugger's `matrix` comparison |
| `shader_seed` | `false` | seed new sessions' shader caches from the last clean stop of the same worker |
| `artifacts` | `{"max_count": 512, "max_bytes": 536870912, "lease_seconds": 60}` | capture storage quota |
| `policy` | see below | budgets and admission |

`policy` defaults: `{"workload": "interactive", "rewind_sessions": null, "rewind_memory_mib": null,
"gpu_capture_sessions": null, "snapshot_slots": null, "snapshot_queue_limit": 16, "snapshot_wait_ms": 10000,
"memory_reserve_mib": 1024, "gpu_admission": true, "gpu_reserve_mib": 0,
"focus_priority": false}`. Interactive defaults allow two rewind sessions,
two GPU captures and two snapshot operations. The rewind memory admission
estimate defaults to 1536 MiB. Batch leaves these budgets unlimited unless
explicitly set. A new session is refused when free
memory is below `memory_reserve_mib` plus the largest running worker. Full
meaning of each: [23](23-local-mcp.md).

Example:

```json
{
  "worker": "C:/LOTUS/runtimes/citnlo/ctr-worker.exe",
  "storage_root": "C:/LOTUS/data/citnlo/sessions",
  "assets": {"demo": "C:/homebrew/demo.3dsx"},
  "asset_directories": {"builds": "C:/src/mygame/build"},
  "allow_control": true,
  "profile": {"graphics_api": 0}
}
```

`"graphics_api": 0` pins software rendering (2 is Vulkan), useful for
reproducible runs. `--asset-dir handle=path` on `lotus-desktop` or `lotus-mcp`
adds or replaces an asset directory without editing the file.

### Worker settings (emulation)

The 173 Azahar settings (CPU clock, region, language, renderer options and so
on). List them with types, bounds, defaults and apply class:

```powershell
bin\ctr-cli.exe settings schema            # everything
bin\ctr-cli.exe settings schema primary    # only what a card may override
```

They resolve in this order, later winning:

1. the worker's default
2. saved global defaults: `<storage_root>/settings/global.json`
3. saved card overrides: `<storage_root>/cards/<card>/lotus-settings.json`
4. a secondary session's temporary overrides (memory only)
5. host `profile` (managed; cannot be changed from a client)

Change them in the Settings window (Ctrl+,), or with the CLI or MCP
(sections 6–7). Saved changes apply to sessions started afterwards, never to a
running one; a running primary lists them as `pending`. A secondary's
temporary overrides apply now, but the worker refuses settings that are not
live while content is loaded. Use JSON values: `100`, `true`, `"text"`.

```powershell
bin\ctr-cli.exe settings set   data\citnlo\host.json global cpu_clock_percentage 150
bin\ctr-cli.exe settings set   data\citnlo\host.json card:main region_value 1
bin\ctr-cli.exe settings unset data\citnlo\host.json card:main region_value
bin\ctr-cli.exe settings reset data\citnlo\host.json card:main
bin\ctr-cli.exe settings show  data\citnlo\host.json card:main
```

`show` prints each effective value with its source (`default`, `global`,
`card`, `runtime`, `managed`) and any problems, such as a saved entry that is
no longer valid. Details: [30](30-sessions-and-interface.md#settings-layers-51).

### Input layouts

Keyboard and controller bindings, stick dead zone, sensitivity and inversion,
and the trigger threshold are `lotus.input.*` settings. They live in the same
files and layers as worker settings, so a card can differ from the global
layout and a secondary can try a change that vanishes when it stops. Unlike
worker settings they apply to running sessions at once and never reach the
emulator. There is no editor yet (#64); use the CLI or MCP:

```powershell
bin\ctr-cli.exe settings set data\citnlo\host.json global lotus.input.preset '"xbox-labels"'
bin\ctr-cli.exe settings set data\citnlo\host.json card:main lotus.input.bind.a '[{"device":"keyboard","key":"j"},{"device":"button","button":"east"}]'
bin\ctr-cli.exe settings set data\citnlo\host.json global lotus.input.circle_pad.deadzone 0.15
```

A binding list replaces that action's bindings; `[]` unbinds it. Keys are the
names the window reports (`z`, `enter`, `up`); controller buttons and axes use
positions (`south`, `east`, `left_x` with `direction` `positive` or
`negative`). Saved presets are managed with `lotus_input_presets` (MCP). An import whose
binding values are not lists is refused; entries this build cannot read are
dropped and listed in the reply's `warnings`. Space, `.`, Tab, Esc and the
modifier keys belong to the window and cannot be bound, and game keys do not
register while Ctrl is held.
Details: [30](30-sessions-and-interface.md#input-layouts-57-63).

### Window preferences (`desktop.json`)

Location: `LOTUS_DESKTOP_PREFERENCES` if set (the portable launcher sets
`<data>/desktop.json`), else `%APPDATA%\LOTUS\desktop.json`. A file that does
not parse is never overwritten; the Settings window says so.

| Key | Default |
| --- | --- |
| `sessions_open`, `sessions_width` | `true`, 264 (200–440 px) |
| `inspector_open`, `inspector_width` | `false`, 420 (300–760 px) |
| `discord_presence`, `discord_hide_titles` | `false`, `false` |
| `discord_application_id` | built-in LOTUS application |
| `split_axis`, `split_ratio` | `auto`, 0.5 (0.2–0.8) |
| `settings_bounds` | `null` (Settings window centred) |

## 5. Running the programs directly

| Program | Usage |
| --- | --- |
| `lotus-desktop` | `lotus-desktop [--mcp] [host.json] [--asset-dir handle=path ...]` |
| `lotus-mcp` | `lotus-mcp [host.json] [--asset-dir handle=path ...]` |
| `ctr-cli` | `ctr-cli help` (section 6) |

Without a `host.json`, `lotus-mcp` reports status and the settings schema but
cannot start workers or read files. Closing the window or disconnecting the MCP
client stops every session: primary cards are kept, secondaries are deleted,
and session IDs expire.

## 6. MCP (for agents)

LOTUS serves the Model Context Protocol over **stdio only**: no network port.
Two ways to run it:

- **Headless**: `lotus-mcp host.json`, or `launch.ps1 -Mode MCP`.
- **Windowed**: `lotus-desktop --mcp host.json`, or `launch.ps1 -Mode
  WindowedMCP`. The agent and you drive the same sessions; you watch and can
  take over. `lotus_status` reports `frontend: windowed`.

Client configuration (Claude Code, Claude Desktop and similar clients use this
shape; adjust paths):

```json
{
  "mcpServers": {
    "lotus": {
      "command": "powershell.exe",
      "args": ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
               "C:/LOTUS/launch.ps1", "-Mode", "WindowedMCP",
               "-Fixture", "C:/homebrew/demo.3dsx"]
    }
  }
}
```

With Claude Code: `claude mcp add lotus -- powershell.exe -NoProfile
-ExecutionPolicy Bypass -File C:/LOTUS/launch.ps1 -Mode WindowedMCP`. To use
your own `host.json`, point `command` at `bin/lotus-mcp.exe` (or
`bin/lotus-desktop.exe` with `--mcp`) and pass the file.

Clients never pass paths or executables. They load content by **asset handle**
(`lotus_status` lists them) and receive captures as `lotus://artifact/...`
resources. Anything that changes state needs `allow_control`.

### Tools

| Tool | Arguments | Does |
| --- | --- | --- |
| `lotus_status` | — | frontend, protocol, sessions, cards, asset handles, limits, policy, artifacts, workspace |
| `lotus_list_sessions` | — | sessions with kind, card/parent, asset and gauges |
| `lotus_list_cards` | — | cards and which session mounts each |
| `lotus_create_session` | `kind` (`secondary` default, `primary`), `card`, `asset` | start a worker; a primary needs `card`; a secondary with `card` copies it; `asset` loads paused |
| `lotus_load` | `session`, `asset` | replace content; paused |
| `lotus_control` | `session`, `operation`: `run`, `pause`, `reset`, `stop` | `run` returns at once and keeps running |
| `lotus_run_until` | `session`, `frames` 1–600 | advance exactly that many frames, then pause |
| `lotus_session_status` | `session` | worker status plus gauges |
| `lotus_input` | `session`, `owner` (`live`), `buttons`, `x`, `y`, `touch`, `circle_pad`, `c_stick` | set the complete input state |
| `lotus_capture_image` | `session`, `scale` (optional, 1–4) | labeled inline PNG images, top then bottom |
| `lotus_capture` | `session` | both screens as raw BGRA artifacts |
| `lotus_registers` | `session`, `core` (0) | r0–r15 and CPSR |
| `lotus_read_memory` | `session`, `address`, `length` 1–4096 | mapped bytes; no writes |
| `lotus_debug` | `session`, `command` | any typed debugger command (section 7) |
| `lotus_gdb` | `session`, `operation` `start`/`stop`, `port` (0 = free) | loopback GDB server (CITnLO) |
| `lotus_focus_session` | `session` | show it in the window; no effect headless |
| `lotus_settings_schema` | `scope` (optional) | the settings registry |
| `lotus_settings` | `target` | effective worker settings with sources |
| `lotus_settings_change` | `target`, `operation` `set`/`unset`/`reset`, `key`, `value` | change saved or temporary settings |
| `lotus_input_presets` | `operation` `list`/`export`/`import`/`duplicate`/`rename`/`delete`, `name`, `to`, `json` | manage input-layout presets |
| `lotus_validate_scenario` | `path` (a host-registered handle) | validate a scenario |
| `lotus_pin_artifact` | `uri`, `pinned` | keep an artifact from eviction |
| `lotus_release_artifacts` | `uris` | delete artifacts now |

`target` is `{"target": "global"}`, `{"target": "card", "card": "main"}` or
`{"target": "session", "session": "<id>"}`.

`buttons` is a bit mask in Azahar's `NativeButton` order
(`src/common/settings.h`): A 0, B 1, X 2, Y 3, Up 4, Down 5, Left 6, Right 7,
L 8, R 9, Start 10, Select 11, ZL 14, ZR 15, Home 16. So A + Right is
`1 + 128 = 129`. Touch `x`/`y` are bottom-screen pixels (320 × 240) with
`touch: true`. Sticks are `[x, y]` in -1..1, x right, y up.

A typical agent loop: `lotus_status` → `lotus_create_session {"kind":
"primary", "card": "main", "asset": "homebrew"}` → `lotus_run_until 60` →
`lotus_capture_image` → `lotus_input` → `lotus_run_until` → … → `lotus_control
stop`.

Raw captures count against the artifact quota: 512 artifacts / 512 MiB by default,
each new one protected for 60 s, then evicted least recently used. Pin what you
need to keep. Policy and quotas: [23](23-local-mcp.md).

## 7. CLI

`ctr-cli` is headless and needs no window.

| Command | Does |
| --- | --- |
| `ctr-cli version` | LOTUS and worker protocol version |
| `ctr-cli pins` | pinned Azahar and GPUI revisions |
| `ctr-cli settings schema [global\|primary\|secondary]` | settings registry as JSON |
| `ctr-cli settings show\|set\|unset\|reset <host.json> global\|card:<name> [key] [json]` | read or change saved settings (section 4) |
| `ctr-cli debug <host.json> <asset-or-dash> [--gdb <port>]` | start one secondary session and read debugger commands on stdin |
| `ctr-cli scenario validate <file>` | check a scenario against the execution limits |
| `ctr-cli scenario run <file> <worker> <out-dir> [repeat]` | run a scenario 1–10 times (default 3) and compare |
| `ctr-cli debug-schema` | JSON schema of every debugger command |
| `ctr-cli mock <worker>` | query a worker's status over private pipes (diagnostics) |

### Interactive debugging

```powershell
bin\ctr-cli.exe debug data\citnlo\host.json homebrew
```

It prints the new session, then takes one JSON command per line and prints one
JSON reply per line. `quit` ends it. `-` instead of an asset starts an empty
session. Commands are objects tagged by `operation`:

```json
{"operation": "status"}
{"operation": "advance", "frames": 60}
{"operation": "registers", "core": 0}
{"operation": "pause"}
```

The full list of 90+ operations (breakpoints, memory, threads, backtraces,
GPU draws, events, savestates, `fork`, rewind, movies, cheats, `matrix` and
more) is in [32](32-debugger-command-parity.md); exact fields come from
`ctr-cli debug-schema`. The same objects work as MCP `lotus_debug`
`command` and as **Run clipboard JSON** in the window. Many need CITnLO.
Background: [07](07-debugger-core.md), [08](08-gpu-and-trace.md).

### GDB

`--gdb 0` (or MCP `lotus_gdb`) starts a loopback GDB remote server on a free
port and prints `GDB listening on 127.0.0.1:<port>`. Connect with devkitARM
`arm-none-eabi-gdb` (`target remote 127.0.0.1:<port>`). While GDB is attached
it holds exclusive control of execution. CITnLO only.

### Scenarios

A scenario is a YAML file: content, profile, inputs at frames, captures, and
assertions (frame hashes, memory, registers, events, code reached). `scenario
run` writes `report.json` and `report.txt` per run and exits non-zero on a
failed assertion. Format and limits: [22](22-scenario-runner.md); examples in
`specs/`.

## 8. Where data lives

Layout under the data directory (`data/` by default, see
[Data directory](#data-directory); `<backend>` is `citnlo` or `azahar`):

| Path | Contents | Kept |
| --- | --- | --- |
| `<backend>/host.json` | generated host configuration | rewritten each start |
| `<backend>/sessions/cards/<card>/` | a card: `sdmc/`, `nand/`, `sysdata/`, `lotus-settings.json`, logs, manifest | yes |
| `<backend>/sessions/scratch/<session>/` | a secondary session | deleted on stop; logs kept after a crash |
| `<backend>/sessions/settings/global.json` | saved global worker settings | yes |
| `<backend>/sessions/shader-seed/` | shared shader caches, if `shader_seed` | yes |
| `desktop.json` | window preferences | yes |

The two backends keep separate data. Cards, saves, logs and captures can
contain personal data; LOTUS sends none of it anywhere. There is no telemetry
or updater.

## 9. Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `Select a .3dsx homebrew file` | the launcher only accepts `.3dsx` |
| Startup fails naming a key and path | a `worker`, `workers` or `assets` path does not exist |
| `profile: ...` at startup | a `profile` key is unknown, excluded, a Lotus key, or out of bounds; check `settings schema` |
| A tool says control is not allowed | set `allow_control: true` |
| New session refused for memory or GPU headroom | close sessions, or lower `policy.memory_reserve_mib` / `gpu_reserve_mib` |
| `Unsupported ... does not support <feature>` | use CITnLO, or a newer worker |
| Session renders in software though Vulkan is preferred | the worker refused Vulkan; the reason is in the session's `renderer_fallback` |
| A card is "locked" | another LOTUS process has its primary open |
| A saved setting has no effect | it applies at the next session start; check `pending` in `settings show` |
| Controller does nothing | pair it in CONTROLLERS; keep a LOTUS window focused |
| Preferences don't save | `desktop.json` does not parse; fix or delete it |
| Artifact URI reads as expired | it was evicted or released; pin artifacts you need |

Worker logs are in the session's folder (`cards/<card>/` or the scratch root).
The inspector can copy a local failure report with log tails to the clipboard.

## 10. Limitations

- Early, unsigned developer build; Windows 11 x64 only.
- Audio is emulated but muted.
- The Settings window cannot edit text, list or binding values, and has no
  search or input-layout editor yet (#54–#58, #64).
- Real controllers and two-monitor DPI handling are untested.
- No hardware-accuracy claim: the workers behave like Azahar.
