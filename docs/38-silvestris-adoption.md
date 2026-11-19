# Silvestris adoption staging (#138)

Branch: silvestris-adoption. This remains opt-in staging, not the default
cutover. The default desktop build keeps its current layout, colours, fonts
and controls. Enabling silvestris-ui registers bundled fonts before the first
window, installs the shared Geist Dark theme, uses Inter/Geist Mono in the
desktop and Settings, and replaces editable overridden Settings reset actions
with the shared small Button. The reset still calls SettingsWindow::write
with None; scope inheritance, validation and persistence retain the same path.
The feature now mounts the shared source list and measured shell, including
overlay/pinned inspector presentation in single and split workspaces.

The emulator/session/controller/MCP crates do not depend on Silvestris.
The input router, worker commands and settings registry retain their existing
interfaces. The measured shell supplies preview-fit dimensions. Staged pin/panel
metadata is namespaced in desktop.json; legacy width/visibility keys remain
authoritative for rollback. Aqua is not enabled in Lotus.

## Source and build

This development branch requires a sibling source layout:

    3DS/silvestris
    3DS/lotus-0.1.0/lotus

config/pins.toml [silvestris] records the source checkout commit.
scripts/verify-silvestris.ps1 checks HEAD, tracked-source cleanliness, and the
shared GPUI/Rust pins. Both use GPUI 6b720973027999bae402df23d469026f4cced988.

Silvestris is not published to a reachable independent GitHub repository yet.
The optional Cargo path dependency still requires that sibling checkout even
for dependency resolution with the feature disabled. This is a staging branch
constraint, not a portable-release solution. Publication/source packaging and
a fetchable immutable dependency are required before merging/releasing it.

On this Windows machine use the GNU toolchain explicitly; the new checkout
does not inherit the original Lotus directory's rustup override:

    scripts/verify-silvestris.ps1
    cargo +1.98.1-x86_64-pc-windows-gnu check -p lotus-desktop --features silvestris-ui --locked
    cargo +1.98.1-x86_64-pc-windows-gnu test -p lotus-desktop --lib --features silvestris-ui --locked
    cargo +1.98.1-x86_64-pc-windows-gnu run -p lotus-desktop --bin lotus-desktop --features silvestris-ui --locked

To roll back the UI slice, build/run without --features silvestris-ui. Legacy
layout keys remain usable and the feature-disabled store preserves the unknown
silvestris object. No guest/card/workspace data migration is involved.

## Validation and rollout gates

The adapter test compares shared Geist Dark surface/text/diagnostic colours
with the existing palette and checks the bundled font roles. Existing
desktop tests cover layout, settings defaults/no-overwrite behavior, touch
transforms and other desktop contracts. These are regression checks, not
proof of native worker/HID/MCP or visual parity.

Before expanding adoption: inspect the opt-in Settings reset row in both
Global and Card scope, compare Inter/Geist Mono clipping against existing
text, and verify keyboard focus and guest input isolation with a live worker.
The shared controls' mock-platform tests live in the Silvestris repository.

Before cutover: migrate source-list/session targeting and persistent rail,
overlay/pinned inspector state and epoch validation, responsive toolbar/status,
menus/commands, Settings chrome, all theme propagation and safe desktop.json
migration. Preserve malformed-file no-overwrite behavior. Complete native
regressions, portable font/art/license packaging, startup/memory benchmarks
and rollback validation before changing the first-run default to Aqua.
Geist stays selectable.

Sources: issues #118/#126/#138; aqua-gpui handbook 19; Silvestris
crates/silvestris-core/src/geist.rs explicitly maps the old Lotus palette.
Local code paths: theme.rs holds the legacy palette, main.rs initializes the
adapter before open_window, settings_window.rs retains the existing write
callback, and preferences.rs owns the unchanged persistence behavior.

Verified compiled/tested on Windows GNU: feature-enabled desktop check and
46 library tests; default desktop 45 library tests. The additional feature
test compares shared/legacy Geist colours and font roles. Native Settings,
live worker/HID/MCP and portable packaging are not verified by these tests.

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
inspector_pinned and inspector_panel (stable string ID). Unknown nested fields
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
