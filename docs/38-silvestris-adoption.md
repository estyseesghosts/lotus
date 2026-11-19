# Silvestris adoption staging (#138)

Branch: silvestris-adoption. This is the initial opt-in adapter, not the shell
cutover. The default desktop build keeps its current layout, colours, fonts
and controls. Enabling silvestris-ui registers bundled fonts before the first
window, installs the shared Geist Dark theme, uses Inter/Geist Mono in the
desktop and Settings, and replaces editable overridden Settings reset actions
with the shared small Button. The reset still calls SettingsWindow::write
with None; scope inheritance, validation and persistence retain the same path.

The emulator/session/controller/MCP crates do not depend on Silvestris.
No input router, preview geometry, worker command, settings registry or
desktop.json schema changes in this slice. Aqua is not enabled in Lotus.

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

To roll back the UI slice, build/run without --features silvestris-ui. No
preference migration or saved state needs reverting.

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
selection/disclosure renderer. These are required before replacing Lotus's
current rows: Start/Stop/Fork/Settings/tiling and workspace-revision drag
checks must stay reachable. The projection is compiled/tested behind
silvestris-ui but is not mounted in LotusWindow yet.

Tests cover saved offline cards, nested/detached copies, selection mapping,
namespace separation, stale epochs, reused session identities and primary
replacement. Silvestris tests cover action/context extension routing and
nonselectable container disclosure. No new worker commands are emitted by
this foundation; main navigation behavior remains on the existing renderer.
