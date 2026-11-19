# Silvestris integration (#138)

Lotus 0.1.0 uses Silvestris for Main and Settings in the default desktop
build. New profiles open in Geist Dark. Settings → General can switch Style
and Appearance live, and Lotus saves the selection in `desktop.json`.
Aqua Light becomes the first-run theme after the release gates and sign-off
in [#138](https://github.com/estyseesghosts/lotus/issues/138).

## Build and rollback

`vendor/silvestris` is an exact Git tree from the independent Silvestris
workspace. `config/pins.toml` records the source commit and tree. The
packagers check the vendored tree and record both IDs in the release
manifest. A clean Lotus checkout and the corresponding-source archive
contain the snapshot. Updating it is a deliberate import from a committed
Silvestris revision, with the pin and lockfile checked in the same change.
The standalone project is still maintained separately.

On Windows GNU:

    ./scripts/verify-silvestris.ps1
    cargo +1.98.1-x86_64-pc-windows-gnu test --locked -p lotus-desktop
    ./scripts/run-silvestris-preview.ps1 -Restart -Theme aqua-light

To build the legacy renderer, use
`cargo build --locked --release -p lotus-desktop --bin lotus-desktop --no-default-features --features wgi`.
Point the resulting desktop at the same `LOTUS_DESKTOP_PREFERENCES` and
host config as the installed build. Visibility and requested widths share
the pre-existing keys. Legacy saves preserve the Silvestris object so the
chosen theme, inspector panel and pin are available on return. Guest
storage and card IDs use the same paths and format.

## Packaging

Both packagers use the default desktop and verify the exact font and artwork
bytes embedded in the executable. The four OFL notices and font provenance
are included as separate files and covered by the release manifest. Runtime
startup uses local assets. The Windows ZIP and Linux AppImage use the
existing native-worker pins and launchers.

## 0.1.0 acceptance evidence

The local candidates were built from Lotus `900cd18` with Rust 1.98.1 and
GPUI `6b720973`. Both reused the released v0.0.7 worker binaries at the
unchanged pins AZAHAR `b04d8a6c` and CITnLO `c3422b0a`. The packagers
checked each native build manifest. The Windows package and Linux AppImage
were relocated and checked for file hashes, dependency resolution, desktop
visibility, CLI, MCP and both workers. The Linux test ran under Xvfb with no
network interface beyond loopback. A separate offline run used nested Weston
and observed the packaged window attach and commit buffers through Wayland.
The source ZIP contains the Silvestris snapshot and its font sources and
notices.

On Windows at 96 DPI, two live CITnLO workers rendered the `t03` homebrew
in a split while all four themes switched in Settings. Both guests continued
running. Keyboard press/release went to its focused tile, touch went to the
other tile, and Actions and Settings consumed their keyboard input. Two virtual Xbox pads
connected through Windows Gaming Input and each sent a press and release to
only its assigned worker. The legacy rollback and reinstalled desktop kept
the saved card, existing pane widths, Settings bounds and Aqua preference.
The packaged AZAHAR focus test saw real Alt+Tab away and back, title-click
activation, minimize and restore. Installed preview followed published
preview within two sequences, matching the legacy baseline's maximum lag.

The Windows workspace suite passed 298 tests and Clippy passed. The native
controller and inspector tests passed separately for each worker, along with
the packaged debugger acceptance and a clean 51-test legacy desktop run.
Windowed MCP split, capture, pause, registers, card persistence and
secondary discard also passed under Linux Xvfb with both packaged workers.

The legacy renderer and Aqua Light produced byte-identical top and bottom
BGRA captures at frames 30, 60 and 120 with the owned debugger fixture on
both native backends. The same checkpoints matched on the `t03` CITnLO
run. On AZAHAR, the first legacy boot of `t03` produced different hashes
from the next legacy boot, which matched Aqua Light at all three
checkpoints. Separate `t03` boots are not a deterministic oracle on AZAHAR.

Startup measurements are medians of five interleaved warm launches on one
Windows 11 machine, measured from process launch to a visible window. With no
host, legacy and Aqua Light took 164.5 and 176.4 ms, used 47.4 and 64.0 MiB
working set, and used 46.9 ms CPU over a two-second idle interval. Resizing
the welcome artwork brought Aqua Light down from about 80 MiB. In the
launcher-configured idle view, legacy and Aqua Light used 51.8 and 64.7 MiB
and took 163.2 and 173.8 ms in the pre-resize build. That view does not load
the welcome artwork.

These results leave an idle-memory increase. Windows startup was checked
with a system-only PATH and clean preferences. Network isolation was verified
on Linux. Physical gamepads, Windows mixed DPI and macOS remain unverified.
The cutover in #138 still requires sign-off. Silvestris is the desktop
implementation and Geist Dark is the first-run theme.

The sections below record the migration in order. Staging comments describe
the state at the time of each step.

## Navigation adapter foundation

navigation.rs projects the current session tree and saved-card names into
generic SourceNodes with separate target metadata. Cards keep stable row IDs
across primary restarts; secondaries retain creation order. Saved offline
cards remain present and expandable, but cannot select a nonexistent primary.
Card/session/group ID namespaces are separate from display labels.

SessionTarget captures ID, creation time and guest epoch. resolve rejects a
missing/replaced session or prior epoch; callers must additionally check each
operation's current availability and control permission. Selection validates
identity/creation time without rejecting a reset of the same session.
A stable card row does not permit an old captured action to target its new
primary. Existing status/FPS label logic is shared from session_tree.rs.

The shared source list now offers accessory and row-decorator callbacks to
preserve app-owned row actions and drag payloads without duplicating its
selection/disclosure renderer. The mounted adapter in
lotus_window/shared_navigation.rs retains Start/Stop/Fork/Settings/tiling
and workspace-revision drag checks.

Tests cover saved offline cards, nested/detached copies, selection mapping,
namespace separation, stale epochs, reused session identities and primary
replacement. Silvestris tests cover action/context extension routing and
nonselectable container disclosure. Worker commands continue through
DesktopController; the default build retains its existing renderer.

## Mounted source list

With silvestris-ui, the expanded sidebar now mounts SourceList. Controller
snapshots synchronize nodes and selection without firing user selection
callbacks. New branches start expanded to preserve visibility of copies;
subsequent polls retain the person's disclosure choice. Target/epoch,
permission and workspace changes invalidate app-owned action snapshots even
when visible text is unchanged. No inspector/session ownership is inferred
from an icon or label.

Start primary, Fork secondary, Card Settings, Stop and Show Beside remain
available as row accessories. Guest mutations check the captured target and
current control permission. Tiling is a UI operation and remains available
in read-only mode; its request carries the workspace revision through
Command::WorkspaceAt. Source row drags retain the existing SessionDrag
payload and drop path. The current collapsed rail, controllers section,
New Session menu, footer and pane divider remain on their existing paths.

Primary/secondary state indicators reuse state_dot and the old palette.
Icon buttons now expose their existing descriptions as accessible button
names. Source input consumes mouse-down at its boundary so accessory clicks
do not return focus to the guest responder. Guest keyboard handling still
requires the main window's focus handle.

Mounted GPUI tests use no host/workers: selecting a copy updates the Lotus
target, disclosure keeps workspace selection, a snapshot update does not
reissue selection, a source row starts a drag, and source/accessory focus
does not hold guest keys. Epoch/permission invalidation is also exercised.
The framework's test-support feature supplies only mock theme/action setup;
production initialization still registers the bundled fonts.

Native worker drag/drop, row-action execution against live sessions, long
label/DPI visual review, expansion persistence and source-list context menus
remain open. No Aqua default change occurs.

## Preference compatibility safeguard

Before adding the shell schema, the desktop store now retains unknown JSON
fields across typed Settings edits and saves. Known values take precedence;
nested future objects and explicit nulls survive unchanged. This implements
the forward-compatibility policy in handbook 19, "Preference migration
strategy", and prepares #138 without changing existing layout keys or defaults.
Any read failure except NotFound disables saving, as malformed JSON already
does. The registry still describes only editable known preferences.

Tests cover future fields, saved widths/bounds, edits through the Settings
serialization path, repeated-save idempotence, invalid known types, non-object
JSON and an I/O read failure. The opt-in desktop suite passes 54 tests and
all-target Clippy passes. The subsequent shell metadata migration is below.

## Measured shell and inspector staging

lotus_window/shared_shell.rs now composes Shell::measured. Its single effective
plan supplies sidebar, workspace fitting, dock/overlay content and rail widths.
The rail is width-dependent in both single and split workspaces; the old
per-tile inspector renderer remains only in the feature-disabled build.
The existing per-tile Inspect request still sets the explicit split target.
Otherwise inspection follows selection, with the target name/short ID shown in
the shell header. While the controller acknowledges a changed target, old
target content is hidden. Inspector row/panel/clipboard actions capture session
ID, creation time and epoch and revalidate identity/permission at activation.
This does not add an expected-epoch field to the controller command queue.

Pin/Unpin, Close, outside-click dismissal, Escape, width dragging and divider
keyboard resizing come from Shell. App menus consume Escape first. Inspector
shortcuts route through the Lotus adapter; remaining keys cannot reach guest
HID. Sidebar/inspector resize callbacks save requested widths and pin state;
divider saves coalesce after 150 ms of inactivity and window close flushes the
latest values. Viewport demotion is never saved. Ctrl+I toggles shared inspector
visibility.

The optional desktop.json silvestris object uses schema_version=1,
inspector_pinned, inspector_panel (stable string ID), theme_family and appearance.
Unknown nested fields
survive. An existing file with inspector_open=true and no silvestris object
migrates to pinned=true to preserve its docked intent (handbook 14/19). New or
previously closed layouts start unpinned. Existing widths, visibility, split
defaults and Settings bounds retain their keys/values. Loading never writes;
the next ordinary save records metadata. Unsupported schema versions or invalid
known metadata disable saving. Feature-disabled builds preserve the object.

Mock GPUI tests verify mounted shell allocation, overlay geometry identity,
pin width reservation, narrow demotion/redocking, split rail/target transition,
Escape ordering, key isolation, outside dismissal and stale-epoch/read-only
action rejection. Preference tests verify explicit migration, unknown metadata,
idempotence and no-overwrite behavior. Native live-screen/touch geometry,
native in-flight debugger response epochs, swap/untile/stop races, native DPI/focus,
resize cancellation and all-theme main/Settings rendering remain unverified.
This is progress toward #131/#132/#138, not their acceptance sign-off.

## Inspector response provenance

The controller now tags register, inspection and surface results with their
captured SessionTarget. Reply epoch tags are checked as well as the latest
reported session metadata and inspected-generation guard. This rejects a reply
from a new guest executed behind a reset even while host telemetry still reports
the old epoch. Host disassembly preserves the underlying read's epoch; a CPU
view spanning two different reported reply epochs is discarded. Formatting
remains off the GPUI thread and outside the shared publication lock.

Publication rechecks target metadata under that lock. Polling keys include
identity/epoch so on-change panels refresh after a reset even while playing.
Cached registers, rows and images are removed when a published target disappears,
changes identity or advances epoch. The window repeats these checks and evicts
uploaded stale images. A different command result also clears an earlier image
from the same epoch. Changing selection alone keeps a valid explicitly inspected
tile's image. Local trace results carry no worker target and cannot inherit a
worker surface. Draw comparisons are scoped to identity/epoch.

Verified compiled/mock: unit regressions cover late replies, cached data,
same-ID replacement, epoch/target changes, removal, explicit reply tags and local
trace provenance. GPUI tests cover result-image retention/replacement and epoch
eviction. The mock-worker controller suite covers reset without a replacement
panel, stalled replies, independent tiled previews, inspected-target swap/untile
and MCP single-view persistence. Native Azahar behavior and UI-thread performance
remain unverified. Queued mutating commands still need an atomic expected-epoch
dispatch guard; reply validation does not prevent a mutation executing late.

## Live theme selection

Global Settings → General → Appearance exposes separate Aqua/Geist and
Light/Dark segmented controls. Selection updates Silvestris's app-global theme,
refreshes all open windows and saves the two dimensions independently inside
the existing silvestris metadata object. Missing fields retain Geist Dark;
unknown enum values disable saving with the existing no-overwrite policy.
The optional --theme startup override initializes both the framework and the
window's preference values; a later ordinary save/close records that choice.

theme.rs now resolves a copyable palette from the active theme for each render.
Main, Settings, gauges, row indicators, tooltips and drag previews receive that
palette explicitly. Permanent source-list icon callbacks resolve it at call
time, rather than keeping their creation theme. Lato/Source Code Pro are used
for Aqua and Inter/Geist Mono for Geist, from the framework Typography tokens.
App surfaces use window colours, keeping dark canvas colours inside preview
frames. Explanatory text uses secondary-text colours rather than disabled ink.
Flat legacy primary actions sample the default button's lower body stop for
contrast; they still need migration to the shared layered button renderer.

References: issues #121/#126/#138; handbook 15/19; Silvestris core theme.rs,
tokens.rs, aqua.rs and geist.rs supply the identities, roles and provenance.
GPUI tests click all four combinations with Main and Settings both open and
check focus, palette/fonts and workspace geometry. A populated split-state test
checks selection, inspected target, panel, source rows and layout preferences
survive switching. Persistence tests cover all combinations, missing/invalid
fields and unknown-field retention. Palette tests check body/secondary/primary
text contrast in every combination. Native pixel, DPI and live-worker theme
switch validation remain open, along with the remaining control/menu
replacement. This does not change the first-run default to Aqua.

## Settings checkbox adoption

With silvestris-ui, Boolean rows use the public Checkbox rather than the old
pill switch. The adjacent descriptor label supplies its accessible name;
the check glyph supplies the visible state. Read-only Boolean rows show a
disabled checkbox and retain their explanation tooltip. Activation still calls
SettingsWindow::write, so registry validation, scope inheritance and persistence
use the existing path. The feature-disabled renderer retains its original switch.

Sources: issue #128/#138; hig-09/specs/checkbox.md (HIG pp. 283–286);
Silvestris docs/checkbox.md records artwork measurements, estimates and keyboard
choices. Mock-platform tests cover pointer/Space changes to a desktop preference,
unrelated-setting retention and a read-only worker setting in all four themes.
Native input, accessibility delivery and pixel/DPI checks remain unverified.

## Unified window composition

Main and Settings now open through chrome::window_options/open_window and render
inside WindowFrame. The framework supplies titlebar materials, traffic lights,
native control hit areas, rounded corners, resize grip and bottom-bar material.
Main uses one window-wide toolbar with shared textured buttons, followed by the
measured content shell and a compact host/session/MCP status strip. Settings has
the same titlebar and frame. Sidebar branding/footer duplication is removed in
staging. The disconnected workspace uses the existing user-provided Lotus icon
(assets/README.md records its source/hash) and a functional Settings action.

The shared toolbar keeps Sidebar, Run/Pause, Step, Open, Actions, Inspector and
Settings reachable at 720 logical px. Zoom/screens controls appear directly at
1060 px; Actions retains the existing zoom/layout and selected-session commands
at narrower sizes. The old command popovers, inspector controls, gauges and
Settings navigation still need conversion; this is not full visual acceptance.

WindowFrame consumes theme-specific title/toolbar/bottom heights. Shell::measured
therefore receives the real remaining content height, and preview fitting no
longer subtracts the old workspace toolbar again. Theme switching preserves
workspace/session state but deliberately recomputes the available preview bounds
from each theme's chrome metrics. Tests verify exact chrome/content boundaries,
all-theme minimum-width control reachability, inspector activation and focus.

Sources: #127/#133/#138, handbook 05/13/19, Silvestris chrome.rs and button.rs.
Windows captures use tools/capture/capture-window.ps1 -WindowOnly -ExactTitle
with the preview PID, allowing inspection even when the terminal or Settings
occludes Main. The initial Aqua Light composition was inspected from its native
1280×880 surface at 100% scale. Other scales/platforms, native window gestures,
screen-reader delivery and live-worker preview/input acceptance remain open.

## Shared command routing

lotus_window/shared_commands.rs registers the staged window's commands in one
silvestris CommandRegistry (#129). Toolbar buttons are CommandButtons and the
Actions button opens a PopupMenu over the same IDs, so label, enabled and
checked state come from a single published descriptor. Global commands
(sidebar, inspector, settings, open, zoom, screens) have fixed IDs. Session
commands (run/pause, step, load, reset, stop) embed the session ID, creation
time and guest epoch; a reset or replaced session re-registers them under new
IDs, the old IDs become Missing, and an open Actions menu is dismissed
(silvestris menus.md requires the consumer to invalidate a replaced target).
Descriptors are republished each render; CommandSet::upsert only bumps its
version on a real change, so this converges without extra frames.

Global shortcuts (Ctrl+B sidebar, Ctrl+I inspector, Ctrl+, settings,
Ctrl+O open) are installed once through CommandRegistry::bindings_where in
the LotusCommands && !SilvestrisTextField context; the root is wrapped in a
CommandScope, so a bound key dispatches the registry command before the
root's key_down. Session commands are left out: their IDs change with each
target, and Space/period are reserved keys key_down handles only while the
window itself is focused.

Load Content reopens the shared PopupMenu with the host's approved assets
(heading "Approved Content") and Open…. Each asset row is a command scoped to
the target's id, creation time and epoch, removed when the target changes,
and its handler rechecks control and the session before sending Load.
The GPUI test actions_menu_shares_toolbar_commands_and_drops_a_replaced_target
covers all four themes.

## Settings as System Preferences

settings_window/shared.rs renders Settings with silvestris preferences.rs
(HIG p. 244): an overview of panes in three bands (Lotus, Emulation,
Development), back/forward and Show All in the toolbar, and the scope popup
("Settings for:") in the toolbar's trailing slot. The window title names the
shown pane. A group with 2 to MAX_TABS subgroups shows them as tabs in a
TabView (HIG pp. 338-341: tabs sized to their labels with 12 px margins,
title-style labels); otherwise subgroups are bold headings in one form.

The window fits the shown pane when the pane or tab changes, never on other
frames: TabView::on_overflow reports the needed size, and fit_size applies it
only while PreferencesState::fit_pending is set. Width stays at least 668 px;
both sides are capped at 66% of the display's visible area, independent of
the window's position. Longer content scrolls with the Aqua scroller
(silvestris controls/scrollbar.rs).

Each row's note says when the setting takes effect; the source files that
read it are a help tag on the control (HIG p. 82). "No host configuration"
and "read-only host" notes appear once at the top of the pane.

Verified in the aqua-light preview: Emulation's five tabs fit at 668 px,
switching to Clock shrinks the window, moving the window does not resize it.
aqua-dark checked by capture (Advanced, Graphics & Display); Geist is not.

## Inspector panel without a rail

The Silvestris UI drops the inspector rail (Shell rail_enabled false). A
closed inspector takes no width; the toolbar Inspector button and Ctrl+I
reopen it. The panel's header names the inspected session, and a pop-up at
its top chooses among the twelve views in Panel::MENU order, grouped as
Panel::group lists them: twelve views exceed a tab view's 2 to about 6
tabs, and HIG p. 341 allows a pop-up in that case. Run/pause and step stay
in the toolbar, so the rail's copies are gone. The legacy (non-silvestris-ui)
build keeps its rail. Ctrl+1..7 and [ / ] still select views.


## Compact controls, Settings search and window separation

Source-list accessories, tile utility actions and inspector commands use
Silvestris icon and small push buttons in the staged build. New Session
and Inspector Utilities use the shared command popup. Opening a popup
releases guest input, commands that create sessions recheck card and
permission state, and clipboard dispatch reuses the existing inspector
target check. Inspector data rows use shared alternating materials and
keep the existing virtualized list. Settings search matches setting keys,
labels, subgroups and group names in the current scope and displays the
existing editable rows. Escape clears a query and restores the pane.

A native Aqua Light desktop capture at 100% DPI confirms Main and Settings
have a visible DWM outline and rounded corners. No exterior shadow was
visible in the overlap capture or the standard Win32 comparison on this
desktop. The framework lets DWM own that effect, including its compositor
and window-state limits. See Silvestris docs/exceptions.md, handbook 06,
and issues #127, #128, #129 and #134.

The opt-in build's regression suite checks the inspector menu's Escape
ordering and Settings search from actual GPUI key events. Aqua cutover
still requires the validation in #138. Some workspace/rail controls and
gauges still use the legacy renderer. Native worker/HID, mixed DPI,
additional OS behavior, portable packaging and performance parity remain
unverified.


## Workspace, settings editors and menu projection

The staged workspace now uses Silvestris notices, rail buttons, content
commands and compact telemetry. Single-view FPS, speed and frame count
sit in the bottom bar. Details discloses four more figures and reserves a
fixed amount of workspace height. Split sessions retain their own labelled
status and controller information in the existing measured edge allocation.
The old toolbar, popovers and gauges compile only for the rollback build.

Actions groups commands by purpose and feeds the same model to GPUI's
native menu on macOS. The macOS Edit menu routes to the focused editor.
Windows and Linux use the in-window menu and native GPUI file pickers.
Source row accessories and right-click commands share target validation.
Stop and Reset open the shared confirmation sheet with Cancel focused.
The sheet traps Tab, suppresses guest input and revalidates the target
before dispatch.

String values and typed JSON or binding lists use the existing registry
validator. Structured values are edited as single-line JSON. The current
scope and editor identity are checked before a draft is committed. Boolean,
choice and numeric rows keep their established validation and save paths.

Inspector requests retain identity and epoch through the desktop command
queue. Supervisor::debug_at checks the revision while holding the worker
mutex before issuing ordinary debugger operations. Host orchestration
operations with separate lock lifetimes perform a pre-dispatch check.
The mock-worker regression rejects an Advance from the pre-reset epoch
and then verifies inspection works against the new epoch.

Release gates in #138 still apply. Windows exterior shadow remains
unverified, along with macOS menu behavior, mixed DPI, live AZAHAR/CITnLO
and portable builds.

## Sidebar collapse and button feedback

The staged workspace takes the full shell width when the session sidebar
closes. The toolbar Sidebar command and its shortcut restore the saved
width. The Settings command stays in the unified toolbar.

Silvestris push, textured and square gradient buttons show the pressed
bevel on pointer or activation-key down. The GPUI click dispatch still
controls command activation. A successful click blends back to the
resting or selected material over 120 ms, a local timing estimate.
Reduced motion keeps the held state and skips the release transition.
See HIG push buttons pp. 265–269 and Aqua GPUI handbook 09 and 10.
