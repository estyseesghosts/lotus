//! Window geometry for the desktop: pane widths, divider drags and the
//! toolbar/gauge density for the space the centre column actually gets.
//!
//! Everything here is in GPUI logical pixels. `Window::viewport_size` is already
//! divided by the display scale, so a 1440 px wide window at 200% is planned as
//! 720 logical pixels; UI scale and window size reduce to the same inputs.
//! Pure functions so the policy is unit-tested without a window.

#[cfg(not(feature = "silvestris-ui"))]
pub const SESSION_RAIL: f32 = 52.0;
/// Silvestris uses the unified toolbar to reopen a fully hidden sidebar.
#[cfg(feature = "silvestris-ui")]
pub const SESSION_RAIL: f32 = 0.0;
pub const SESSION_MIN: f32 = 200.0;
pub const SESSION_MAX: f32 = 440.0;
pub const SESSION_DEFAULT: f32 = 264.0;
/// Inspector quick-access rail; always shown, outermost on the right.
pub const INSPECTOR_RAIL: f32 = 44.0;
pub const INSPECTOR_MIN: f32 = 300.0;
pub const INSPECTOR_MAX: f32 = 760.0;
pub const INSPECTOR_DEFAULT: f32 = 420.0;
/// Narrowest centre column: the compact toolbar (label, Run, Step, overflow)
/// and Fit screens at their 0.5 minimum (200 px wide stacked) still fit.
pub const MAIN_MIN: f32 = 320.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sessions,
    Inspector,
}

/// A pane's user-chosen state. `width` is kept while closed so reopening
/// restores it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneState {
    pub open: bool,
    pub width: f32,
}

/// What is rendered for one viewport width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    /// Session sidebar width when shown expanded.
    pub sessions: Option<f32>,
    /// Inspector panel width beside its rail, `None` when only the rail shows.
    pub inspector: Option<f32>,
    /// A pane the user left open that is drawn as its rail for lack of space.
    pub squeezed: Option<Pane>,
    /// False in a split: the inspector rail and panel are gone, width included (lotus#71).
    pub inspector_rail: bool,
    pub main: f32,
}

impl Plan {
    #[must_use]
    pub fn sessions_width(&self) -> f32 {
        self.sessions.unwrap_or(SESSION_RAIL)
    }
    #[must_use]
    pub fn inspector_width(&self) -> f32 {
        if self.inspector_rail {
            INSPECTOR_RAIL + self.inspector.unwrap_or(0.0)
        } else {
            0.0
        }
    }
}

fn limits(pane: Pane) -> (f32, f32) {
    match pane {
        Pane::Sessions => (SESSION_MIN, SESSION_MAX),
        Pane::Inspector => (INSPECTOR_MIN, INSPECTOR_MAX),
    }
}

/// Fit both panes and a centre column of at least [`MAIN_MIN`]. Open panes
/// shrink toward their minimum, the less recently opened one first. If that
/// is still too wide, the less recently opened pane is drawn as its rail
/// (`squeezed`) without changing its saved state, so it comes back when the
/// window grows. The most recently opened pane is never squeezed.
#[must_use]
pub fn plan(viewport: f32, sessions: PaneState, inspector: PaneState, recent: Pane) -> Plan {
    plan_with(viewport, sessions, Some(inspector), recent)
}

/// [`plan`] with the inspector rail removed (`inspector` is `None`), as in a split
/// workspace. The saved inspector state is untouched and returns with single view.
#[must_use]
pub fn plan_with(
    viewport: f32,
    sessions: PaneState,
    inspector: Option<PaneState>,
    recent: Pane,
) -> Plan {
    let rail = inspector.is_some();
    let mut s = sessions
        .open
        .then(|| sessions.width.clamp(SESSION_MIN, SESSION_MAX));
    let mut i = inspector
        .filter(|inspector| inspector.open)
        .map(|inspector| inspector.width.clamp(INSPECTOR_MIN, INSPECTOR_MAX));
    let rail_width = if rail { INSPECTOR_RAIL } else { 0.0 };
    let main = |s: Option<f32>, i: Option<f32>| {
        viewport - s.unwrap_or(SESSION_RAIL) - rail_width - i.unwrap_or(0.0)
    };
    let order = match recent {
        Pane::Sessions => [Pane::Inspector, Pane::Sessions],
        Pane::Inspector => [Pane::Sessions, Pane::Inspector],
    };
    for pane in order {
        let deficit = MAIN_MIN - main(s, i);
        if deficit <= 0.0 {
            break;
        }
        let (min, _) = limits(pane);
        let width = match pane {
            Pane::Sessions => &mut s,
            Pane::Inspector => &mut i,
        };
        if let Some(width) = width {
            *width = (*width - deficit).max(min);
        }
    }
    let mut squeezed = None;
    if main(s, i) < MAIN_MIN {
        let victim = order[0];
        let width = match victim {
            Pane::Sessions => &mut s,
            Pane::Inspector => &mut i,
        };
        if width.take().is_some() {
            squeezed = Some(victim);
        }
    }
    Plan {
        sessions: s,
        inspector: i,
        squeezed,
        inspector_rail: rail,
        main: main(s, i),
    }
}

/// An in-progress divider drag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drag {
    pub pane: Pane,
    /// Saved width when the drag began; restored if the drag snaps closed.
    pub start_width: f32,
}

/// Pane state for a divider at window x `pointer`.
///
/// The proposed width is the pointer's distance from the pane's window edge.
/// Below half the minimum the pane snaps closed; at or above it, it is open
/// and clamped to `[min, max]`, where `max` also leaves [`MAIN_MIN`] for the
/// centre column beside `other` (the other pane's rendered width). Same
/// threshold in both directions, as VS Code's split view does
/// (`src/vs/base/browser/ui/splitview/splitview.ts`, `onSashStart`:
/// `halfSize = Math.floor(viewItem.viewMinimumSize / 2)`), so ordinary
/// resizing above the minimum never sticks.
#[must_use]
pub fn drag(drag: Drag, pointer: f32, viewport: f32, other: f32) -> PaneState {
    let (min, max) = limits(drag.pane);
    let (proposed, rail) = match drag.pane {
        Pane::Sessions => (pointer, 0.0),
        Pane::Inspector => (viewport - pointer - INSPECTOR_RAIL, INSPECTOR_RAIL),
    };
    if proposed < min / 2.0 {
        return PaneState {
            open: false,
            width: drag.start_width,
        };
    }
    let room = viewport - other - rail - MAIN_MIN;
    PaneState {
        open: true,
        width: proposed.min(max).min(room).max(min),
    }
}

/// How much of the session toolbar fits in the centre column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolbarDensity {
    /// Run/Pause, Step and an overflow menu holding every other action.
    Compact,
    /// Run/Pause labelled, other actions as icons with tooltips.
    Icons,
    /// Every action labelled, plus session detail text.
    Full,
}

/// From `tools/desktop_shots.py` captures (Windows 11, Segoe UI 14 px, 100%):
/// the labelled controls span 476 px (Pause to "Stacked", `wide-default`); with
/// the session label, badge, gaps and 32 px padding the bar needs about 670 px,
/// so 780 leaves about 110 px of detail text. The icon controls span 337 px
/// (`wide-inspector`); with a label truncated to its 48 px minimum, the badge,
/// gaps and padding that is about 505 px.
pub const TOOLBAR_FULL_MIN: f32 = 780.0;
pub const TOOLBAR_ICONS_MIN: f32 = 520.0;

#[must_use]
pub fn toolbar_density(main: f32) -> ToolbarDensity {
    if main >= TOOLBAR_FULL_MIN {
        ToolbarDensity::Full
    } else if main >= TOOLBAR_ICONS_MIN {
        ToolbarDensity::Icons
    } else {
        ToolbarDensity::Compact
    }
}

/// Gauge strip presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GaugeMode {
    /// One row of seven cards with label, figure and detail.
    Cards,
    /// Fixed-width label/value pills: FPS, speed and frame, the rest behind a
    /// disclosure.
    Compact { expanded: bool },
}

/// Cards need this much: the FPS card (184) and six cards at their 112 px
/// minimum, 8 px gaps and 12 px padding each side.
pub const CARDS_MIN_WIDTH: f32 = 184.0 + 6.0 * 112.0 + 6.0 * 8.0 + 24.0;
/// Below this viewport height the screens get the space instead of cards.
pub const CARDS_MIN_HEIGHT: f32 = 700.0;
pub const CARDS_HEIGHT: f32 = 100.0;
pub const PILL_HEIGHT: f32 = 28.0;
pub const PILL_GAP: f32 = 6.0;
/// Strip padding: 8 px each side, top and bottom, plus its top border.
pub const PILL_PADDING: f32 = 8.0;
const PILL_STRIP_CHROME: f32 = 2.0 * PILL_PADDING + 1.0;
/// The disclosure button at the end of the first row.
pub const DISCLOSURE_WIDTH: f32 = 24.0;
/// Disclosed pills (renderer names, memory) need this much to stay legible.
pub const SECONDARY_PILL_MIN: f32 = 132.0;
pub const PRIMARY_GAUGES: usize = 3;
pub const ALL_GAUGES: usize = 7;

#[must_use]
pub fn gauge_mode(main: f32, viewport_height: f32, expanded: bool) -> GaugeMode {
    if main >= CARDS_MIN_WIDTH && viewport_height >= CARDS_MIN_HEIGHT {
        GaugeMode::Cards
    } else {
        GaugeMode::Compact { expanded }
    }
}

/// Compact strip geometry: the three primary pills share the first row with
/// the disclosure button (at [`MAIN_MIN`] each is about 87 px, enough for
/// "SPEED 100%"); disclosed pills fill full-width rows below.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pills {
    pub primary_width: f32,
    pub secondary_per_row: usize,
    pub secondary_width: f32,
}

#[must_use]
pub fn pills(main: f32) -> Pills {
    let row = main - 2.0 * PILL_PADDING;
    let first = row - DISCLOSURE_WIDTH - PILL_GAP;
    let primary = PRIMARY_GAUGES as f32;
    let secondary = (ALL_GAUGES - PRIMARY_GAUGES) as f32;
    let per_row = ((row + PILL_GAP) / (SECONDARY_PILL_MIN + PILL_GAP))
        .floor()
        .clamp(1.0, secondary);
    Pills {
        primary_width: (first - (primary - 1.0) * PILL_GAP) / primary,
        secondary_per_row: per_row as usize,
        secondary_width: (row - (per_row - 1.0) * PILL_GAP) / per_row,
    }
}

/// Height the gauge strip takes, used to fit the screens.
#[must_use]
pub fn gauge_height(mode: GaugeMode, main: f32) -> f32 {
    match mode {
        GaugeMode::Cards => CARDS_HEIGHT,
        GaugeMode::Compact { expanded } => {
            let extra = if expanded {
                (ALL_GAUGES - PRIMARY_GAUGES).div_ceil(pills(main).secondary_per_row) as f32
            } else {
                0.0
            };
            PILL_STRIP_CHROME + PILL_HEIGHT + extra * (PILL_HEIGHT + PILL_GAP)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPEN_S: PaneState = PaneState {
        open: true,
        width: SESSION_DEFAULT,
    };
    const OPEN_I: PaneState = PaneState {
        open: true,
        width: INSPECTOR_DEFAULT,
    };
    const CLOSED_I: PaneState = PaneState {
        open: false,
        width: INSPECTOR_DEFAULT,
    };

    #[test]
    fn wide_windows_keep_saved_widths() {
        let plan = plan(1600.0, OPEN_S, OPEN_I, Pane::Inspector);
        assert_eq!(plan.sessions, Some(SESSION_DEFAULT));
        assert_eq!(plan.inspector, Some(INSPECTOR_DEFAULT));
        assert_eq!(plan.squeezed, None);
        assert_eq!(plan.main, 1600.0 - 264.0 - 44.0 - 420.0);
    }

    #[test]
    fn narrowing_shrinks_the_older_pane_first_then_squeezes_it() {
        // 1000: needs 1000-264-44-420 = 272 -> 48 short; sessions (older) gives it.
        // lotus#71: a split drops the rail and panel and gives their width to the stage.
        let split = plan_with(1600.0, OPEN_S, None, Pane::Inspector);
        assert!(split.inspector.is_none() && !split.inspector_rail);
        assert_eq!(split.inspector_width(), 0.0);
        assert_eq!(split.main, 1600.0 - split.sessions_width());
        let p = plan(1000.0, OPEN_S, OPEN_I, Pane::Inspector);
        assert_eq!(p.sessions, Some(SESSION_DEFAULT - 48.0));
        assert_eq!(p.inspector, Some(INSPECTOR_DEFAULT));
        // 900: sessions at its minimum is not enough; inspector shrinks next.
        let p = plan(900.0, OPEN_S, OPEN_I, Pane::Inspector);
        assert_eq!(p.sessions, Some(SESSION_MIN));
        assert_eq!(p.main, MAIN_MIN);
        // 720 (the window minimum): both minimums do not fit; sessions becomes its rail.
        let p = plan(720.0, OPEN_S, OPEN_I, Pane::Inspector);
        assert_eq!(p.sessions, None);
        assert_eq!(p.squeezed, Some(Pane::Sessions));
        assert!(p.inspector.is_some());
        assert!(p.main >= MAIN_MIN, "{p:?}");
        // Opening the sidebar last swaps which pane yields.
        let p = plan(720.0, OPEN_S, OPEN_I, Pane::Sessions);
        assert_eq!(p.squeezed, Some(Pane::Inspector));
        assert!(p.sessions.is_some());
    }

    #[test]
    fn the_window_minimum_fits_every_single_open_pane() {
        for (s, i) in [
            (OPEN_S, CLOSED_I),
            (
                PaneState {
                    open: false,
                    ..OPEN_S
                },
                OPEN_I,
            ),
        ] {
            let p = plan(720.0, s, i, Pane::Inspector);
            assert_eq!(p.squeezed, None, "{p:?}");
            assert!(p.main >= MAIN_MIN, "{p:?}");
        }
    }

    #[test]
    fn dragging_snaps_at_half_the_minimum_in_both_directions() {
        let d = Drag {
            pane: Pane::Sessions,
            start_width: 300.0,
        };
        assert_eq!(
            drag(d, 99.0, 1600.0, 464.0),
            PaneState {
                open: false,
                width: 300.0
            }
        );
        assert_eq!(drag(d, 100.0, 1600.0, 464.0).width, SESSION_MIN);
        assert!(drag(d, 100.0, 1600.0, 464.0).open);
        assert_eq!(drag(d, 333.0, 1600.0, 464.0).width, 333.0);
        assert_eq!(drag(d, 900.0, 1600.0, 464.0).width, SESSION_MAX);
        // Never past the centre minimum: 1000 - 464 - 320 = 216.
        assert_eq!(drag(d, 400.0, 1000.0, 464.0).width, 216.0);
        let d = Drag {
            pane: Pane::Inspector,
            start_width: 500.0,
        };
        // Pointer 1200 in a 1600 window: 1600 - 1200 - 44 = 356 wide.
        assert_eq!(drag(d, 1200.0, 1600.0, 264.0).width, 356.0);
        assert!(!drag(d, 1600.0 - 44.0 - 149.0, 1600.0, 264.0).open);
        assert!(drag(d, 1600.0 - 44.0 - 150.0, 1600.0, 264.0).open);
    }

    #[test]
    fn densities_follow_the_centre_width() {
        assert_eq!(toolbar_density(1000.0), ToolbarDensity::Full);
        assert_eq!(toolbar_density(600.0), ToolbarDensity::Icons);
        assert_eq!(toolbar_density(MAIN_MIN), ToolbarDensity::Compact);
        assert_eq!(gauge_mode(1100.0, 900.0, false), GaugeMode::Cards);
        assert_eq!(
            gauge_mode(1100.0, 600.0, true),
            GaugeMode::Compact { expanded: true }
        );
        let narrow = pills(MAIN_MIN);
        assert!(narrow.primary_width > 86.0, "{narrow:?}");
        assert_eq!(narrow.secondary_per_row, 2);
        assert_eq!(pills(800.0).secondary_per_row, 4);
        assert_eq!(
            gauge_height(GaugeMode::Compact { expanded: false }, 800.0),
            45.0
        );
        assert_eq!(
            gauge_height(GaugeMode::Compact { expanded: true }, 800.0),
            79.0
        );
        // Four disclosed pills at two per row: two more rows.
        assert_eq!(
            gauge_height(GaugeMode::Compact { expanded: true }, MAIN_MIN),
            113.0
        );
    }
}
