//! Window workspace: which sessions a windowed LOTUS shows and how (lotus#67).
//!
//! One authoritative presentation state, changed through [`Workspace::apply`] by both the
//! desktop (sidebar, drag and drop) and MCP. It is host/window state only: applying an
//! operation never starts, stops, pauses or reconfigures a worker and never changes the
//! inspector target, controller assignment or audio focus, which are separate state.
//!
//! Sessions are stored by their stable session ID. `Horizontal` is first-left/second-right,
//! `Vertical` first-top/second-bottom; `Auto` lets the window pick from its aspect ratio.
//! Every committed change increments `revision`; an operation that changes nothing commits
//! nothing. With `expected_revision` an operation is refused once the state has moved on, so
//! an agent cannot overwrite a newer arrangement made by a person.

use serde::{Deserialize, Serialize};

/// Bounds of the first tile's share of the split axis. Policy, not hardware: below a fifth a
/// 400×240 top screen in a 1280-wide window is under 256 px wide.
pub const RATIO_MIN: f32 = 0.2;
pub const RATIO_MAX: f32 = 0.8;
pub const RATIO_DEFAULT: f32 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    #[default]
    Auto,
    /// First left, second right.
    Horizontal,
    /// First top, second bottom.
    Vertical,
}

/// How one tile arranges a console's top and bottom screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenLayout {
    #[default]
    Auto,
    Stacked,
    SideBySide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Viewport {
    #[default]
    First,
    Second,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Layout {
    /// Nothing shown.
    #[default]
    Empty,
    Single {
        session: String,
        screens: ScreenLayout,
    },
    Split {
        first: String,
        second: String,
        axis: Axis,
        /// The first tile's share of the split axis, in `RATIO_MIN..=RATIO_MAX`.
        ratio: f32,
        screens: [ScreenLayout; 2],
    },
}

impl Layout {
    /// Sessions shown, in tile order.
    #[must_use]
    pub fn visible(&self) -> Vec<&str> {
        match self {
            Self::Empty => Vec::new(),
            Self::Single { session, .. } => vec![session],
            Self::Split { first, second, .. } => vec![first, second],
        }
    }

    /// The screen arrangement `session` is shown with, if it is visible.
    #[must_use]
    pub fn screens_of(&self, session: &str) -> Option<ScreenLayout> {
        match self {
            Self::Single {
                session: s,
                screens,
            } if s == session => Some(*screens),
            Self::Split {
                first,
                second,
                screens,
                ..
            } if first == session || second == session => {
                Some(screens[usize::from(second == session)])
            }
            _ => None,
        }
    }

    /// The session in `viewport`; a single view has only the first.
    #[must_use]
    pub fn session_at(&self, viewport: Viewport) -> Option<&str> {
        match (self, viewport) {
            (Self::Single { session, .. }, Viewport::First)
            | (Self::Split { first: session, .. }, Viewport::First)
            | (
                Self::Split {
                    second: session, ..
                },
                Viewport::Second,
            ) => Some(session),
            _ => None,
        }
    }
}

/// Split settings used when a `tile` leaves them out; the desktop persists them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SplitDefaults {
    pub axis: Axis,
    pub ratio: f32,
}

impl Default for SplitDefaults {
    fn default() -> Self {
        Self {
            axis: Axis::Auto,
            ratio: RATIO_DEFAULT,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: u64,
    pub layout: Layout,
    /// Tile receiving keyboard and touch input; always `First` outside a split.
    pub focused_viewport: Viewport,
}

impl Snapshot {
    /// The session keyboard input goes to: the focused tile's, or the single view's.
    #[must_use]
    pub fn focused_session(&self) -> Option<&str> {
        self.layout.session_at(self.focused_viewport)
    }
}

/// What `lotus_status.workspace` and `lotus_workspace get` report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    /// False on a headless host: there is no window and every change is refused.
    pub available: bool,
    #[serde(flatten)]
    pub snapshot: Snapshot,
    pub defaults: SplitDefaults,
    /// What the window actually drew for this revision (lotus#69); `None` until it lays
    /// the split out, outside a split, or when the report is for an older revision.
    pub resolved: Option<Resolved>,
}

/// The window's resolved split: `Auto` replaced by the chosen axis and screen
/// arrangement, and the guest-pixel scale of each tile.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Resolved {
    /// Workspace revision this layout was computed for.
    pub revision: u64,
    /// `Horizontal` or `Vertical`, never `Auto`.
    pub axis: Axis,
    /// `Stacked` or `SideBySide`, never `Auto`.
    pub screens: [ScreenLayout; 2],
    /// Host pixels per guest pixel, per tile.
    pub scale: [f32; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// Show two distinct sessions side by side; unset fields keep the current split's value,
    /// or the defaults when not split.
    Tile {
        first: String,
        second: String,
        #[serde(default)]
        axis: Option<Axis>,
        #[serde(default)]
        ratio: Option<f32>,
        #[serde(default)]
        screens: Option<[ScreenLayout; 2]>,
    },
    /// Show one session alone. From a split, the other tile's session returns to the sidebar.
    Single {
        session: String,
    },
    /// Leave a split showing `keep`, or the first tile when unset.
    Untile {
        #[serde(default)]
        keep: Option<String>,
    },
    Swap,
    SetAxis {
        axis: Axis,
    },
    SetRatio {
        ratio: f32,
    },
    SetScreenLayout {
        viewport: Viewport,
        screens: ScreenLayout,
    },
    FocusViewport {
        viewport: Viewport,
    },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WorkspaceError {
    #[error("workspace changed: expected revision {expected}, current {current}; read it again")]
    Stale { expected: u64, current: u64 },
    #[error("no session {0}")]
    UnknownSession(String),
    #[error("a split needs two different sessions, both were {0}")]
    Duplicate(String),
    #[error("split ratio {0} outside {RATIO_MIN}..={RATIO_MAX}")]
    Ratio(f32),
    #[error("the workspace is not split")]
    NotSplit,
    #[error("{0} is not shown in the split")]
    NotVisible(String),
}

#[derive(Debug, Clone)]
pub struct Workspace {
    revision: u64,
    layout: Layout,
    focused: Viewport,
    defaults: SplitDefaults,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            revision: 0,
            layout: Layout::Empty,
            focused: Viewport::First,
            defaults: SplitDefaults::default(),
        }
    }
}

fn check_ratio(ratio: f32) -> Result<f32, WorkspaceError> {
    if (RATIO_MIN..=RATIO_MAX).contains(&ratio) {
        Ok(ratio)
    } else {
        Err(WorkspaceError::Ratio(ratio))
    }
}

impl Workspace {
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            layout: self.layout.clone(),
            focused_viewport: self.focused,
        }
    }

    #[must_use]
    pub fn defaults(&self) -> SplitDefaults {
        self.defaults
    }

    /// Replaces the split defaults; out-of-range ratios fall back to the default.
    pub fn set_defaults(&mut self, defaults: SplitDefaults) {
        self.defaults = SplitDefaults {
            axis: defaults.axis,
            ratio: check_ratio(defaults.ratio).unwrap_or(RATIO_DEFAULT),
        };
    }

    /// Validates `operation` against the current state and `live` (whether a session ID
    /// exists), then commits it whole or not at all.
    pub fn apply(
        &mut self,
        operation: &Operation,
        expected_revision: Option<u64>,
        live: impl Fn(&str) -> bool,
    ) -> Result<Snapshot, WorkspaceError> {
        if let Some(expected) = expected_revision.filter(|e| *e != self.revision) {
            return Err(WorkspaceError::Stale {
                expected,
                current: self.revision,
            });
        }
        let known = |id: &str| {
            if live(id) {
                Ok(())
            } else {
                Err(WorkspaceError::UnknownSession(id.to_string()))
            }
        };
        let mut layout = self.layout.clone();
        let mut focused = self.focused;
        match operation {
            Operation::Tile {
                first,
                second,
                axis,
                ratio,
                screens,
            } => {
                known(first)?;
                known(second)?;
                if first == second {
                    return Err(WorkspaceError::Duplicate(first.clone()));
                }
                let ratio = ratio.map(check_ratio).transpose()?;
                let (current_axis, current_ratio) = match &layout {
                    Layout::Split { axis, ratio, .. } => (*axis, *ratio),
                    _ => (self.defaults.axis, self.defaults.ratio),
                };
                // Screen arrangement and keyboard focus belong to sessions, not slots, so a
                // reorder carries them (#110). A session new to the workspace starts `auto`;
                // if the focused session left, focus stays on its slot.
                let carried = [
                    layout.screens_of(first).unwrap_or(ScreenLayout::Auto),
                    layout.screens_of(second).unwrap_or(ScreenLayout::Auto),
                ];
                let focused_session = layout.session_at(focused).map(str::to_string);
                if focused_session.as_deref() == Some(first.as_str()) {
                    focused = Viewport::First;
                } else if focused_session.as_deref() == Some(second.as_str()) {
                    focused = Viewport::Second;
                }
                layout = Layout::Split {
                    first: first.clone(),
                    second: second.clone(),
                    axis: axis.unwrap_or(current_axis),
                    ratio: ratio.unwrap_or(current_ratio),
                    screens: screens.unwrap_or(carried),
                };
            }
            Operation::Single { session } => {
                known(session)?;
                let screens = layout.screens_of(session).unwrap_or(ScreenLayout::Auto);
                layout = Layout::Single {
                    session: session.clone(),
                    screens,
                };
                focused = Viewport::First;
            }
            Operation::Untile { keep } => {
                let Layout::Split {
                    first,
                    second,
                    screens,
                    ..
                } = &layout
                else {
                    return Err(WorkspaceError::NotSplit);
                };
                let keep = keep.as_deref().unwrap_or(first);
                let index = if keep == first {
                    0
                } else if keep == second {
                    1
                } else {
                    return Err(WorkspaceError::NotVisible(keep.to_string()));
                };
                layout = Layout::Single {
                    session: keep.to_string(),
                    screens: screens[index],
                };
                focused = Viewport::First;
            }
            Operation::Swap => {
                let Layout::Split {
                    first,
                    second,
                    screens,
                    ratio,
                    ..
                } = &mut layout
                else {
                    return Err(WorkspaceError::NotSplit);
                };
                std::mem::swap(first, second);
                screens.swap(0, 1);
                *ratio = 1.0 - *ratio;
                // The focused session keeps focus in its new tile.
                focused = match focused {
                    Viewport::First => Viewport::Second,
                    Viewport::Second => Viewport::First,
                };
            }
            Operation::SetAxis { axis: new } => {
                let Layout::Split { axis, .. } = &mut layout else {
                    return Err(WorkspaceError::NotSplit);
                };
                *axis = *new;
            }
            Operation::SetRatio { ratio: new } => {
                let new = check_ratio(*new)?;
                let Layout::Split { ratio, .. } = &mut layout else {
                    return Err(WorkspaceError::NotSplit);
                };
                *ratio = new;
            }
            Operation::SetScreenLayout { viewport, screens } => match &mut layout {
                Layout::Split { screens: tiles, .. } => {
                    tiles[usize::from(*viewport == Viewport::Second)] = *screens;
                }
                Layout::Single { screens: tile, .. } if *viewport == Viewport::First => {
                    *tile = *screens;
                }
                _ => return Err(WorkspaceError::NotSplit),
            },
            Operation::FocusViewport { viewport } => {
                if *viewport == Viewport::Second && !matches!(layout, Layout::Split { .. }) {
                    return Err(WorkspaceError::NotSplit);
                }
                focused = *viewport;
            }
        }
        if layout != self.layout || focused != self.focused {
            self.layout = layout;
            self.focused = focused;
            self.revision += 1;
        }
        Ok(self.snapshot())
    }

    /// Fallback when a session is gone (stopped or removed; a crashed session stays
    /// registered and keeps its tile). A split collapses to the remaining tile without
    /// touching that session's worker; a single view becomes empty. Commits only on change.
    pub fn remove(&mut self, id: &str) -> bool {
        let layout = match &self.layout {
            Layout::Single { session, .. } if session == id => Layout::Empty,
            Layout::Split {
                first,
                second,
                screens,
                ..
            } if first == id || second == id => {
                let index = usize::from(first == id);
                Layout::Single {
                    session: if first == id { second } else { first }.clone(),
                    screens: screens[index],
                }
            }
            _ => return false,
        };
        self.layout = layout;
        self.focused = Viewport::First;
        self.revision += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(id: &str) -> bool {
        ["a", "b", "c"].contains(&id)
    }

    fn tile(first: &str, second: &str) -> Operation {
        Operation::Tile {
            first: first.into(),
            second: second.into(),
            axis: None,
            ratio: None,
            screens: None,
        }
    }

    #[test]
    fn tile_carries_focus_and_screens_by_session() {
        let mut w = Workspace::default();
        w.apply(&tile("a", "b"), None, live).unwrap();
        w.apply(
            &Operation::SetScreenLayout {
                viewport: Viewport::First,
                screens: ScreenLayout::Stacked,
            },
            None,
            live,
        )
        .unwrap();
        w.apply(
            &Operation::SetScreenLayout {
                viewport: Viewport::Second,
                screens: ScreenLayout::SideBySide,
            },
            None,
            live,
        )
        .unwrap();
        w.apply(
            &Operation::FocusViewport {
                viewport: Viewport::First,
            },
            None,
            live,
        )
        .unwrap();
        // Reorder: A keeps focus and stacked screens in the second tile.
        let s = w.apply(&tile("b", "a"), None, live).unwrap();
        assert_eq!(s.layout.session_at(s.focused_viewport), Some("a"));
        assert_eq!(s.layout.screens_of("a"), Some(ScreenLayout::Stacked));
        assert_eq!(s.layout.screens_of("b"), Some(ScreenLayout::SideBySide));
        // Replacing the unfocused B: A keeps everything, C starts auto.
        let s = w.apply(&tile("c", "a"), None, live).unwrap();
        assert_eq!(s.layout.session_at(s.focused_viewport), Some("a"));
        assert_eq!(s.layout.screens_of("a"), Some(ScreenLayout::Stacked));
        assert_eq!(s.layout.screens_of("c"), Some(ScreenLayout::Auto));
        // Replacing the focused A: focus stays on its slot, which B now fills.
        let s = w.apply(&tile("c", "b"), None, live).unwrap();
        assert_eq!(s.focused_viewport, Viewport::Second);
        assert_eq!(s.layout.session_at(s.focused_viewport), Some("b"));
        // An explicit `screens` still wins.
        let s = w
            .apply(
                &Operation::Tile {
                    first: "b".into(),
                    second: "c".into(),
                    axis: None,
                    ratio: None,
                    screens: Some([ScreenLayout::Auto, ScreenLayout::Stacked]),
                },
                None,
                live,
            )
            .unwrap();
        assert_eq!(s.layout.screens_of("c"), Some(ScreenLayout::Stacked));
        // From a single view, the shown session keeps focus and screens wherever it lands.
        w.apply(
            &Operation::Single {
                session: "a".into(),
            },
            None,
            live,
        )
        .unwrap();
        let s = w.apply(&tile("b", "a"), None, live).unwrap();
        assert_eq!(s.layout.session_at(s.focused_viewport), Some("a"));
    }

    #[test]
    fn transitions_commit_and_count_revisions() {
        let mut w = Workspace::default();
        let s = w
            .apply(
                &Operation::Single {
                    session: "a".into(),
                },
                None,
                live,
            )
            .unwrap();
        assert_eq!(s.revision, 1);
        let s = w.apply(&tile("a", "b"), Some(1), live).unwrap();
        assert_eq!(s.revision, 2);
        assert_eq!(s.layout.visible(), ["a", "b"]);
        // Idempotent: the same tile again commits nothing.
        assert_eq!(w.apply(&tile("a", "b"), None, live).unwrap().revision, 2);
        let s = w
            .apply(&Operation::SetRatio { ratio: 0.3 }, None, live)
            .unwrap();
        let s2 = w.apply(&Operation::Swap, Some(s.revision), live).unwrap();
        let Layout::Split { first, ratio, .. } = &s2.layout else {
            panic!()
        };
        assert_eq!(first, "b");
        assert!((ratio - 0.7).abs() < 1e-6);
        assert_eq!(s2.focused_viewport, Viewport::Second);
        let s3 = w
            .apply(
                &Operation::Untile {
                    keep: Some("a".into()),
                },
                None,
                live,
            )
            .unwrap();
        assert_eq!(s3.layout.visible(), ["a"]);
        assert_eq!(s3.focused_viewport, Viewport::First);
    }

    #[test]
    fn invalid_requests_change_nothing() {
        let mut w = Workspace::default();
        w.apply(&tile("a", "b"), None, live).unwrap();
        let before = w.snapshot();
        for (op, expected) in [
            (tile("a", "a"), WorkspaceError::Duplicate("a".into())),
            (tile("a", "z"), WorkspaceError::UnknownSession("z".into())),
            (
                Operation::Tile {
                    first: "a".into(),
                    second: "c".into(),
                    axis: None,
                    ratio: Some(0.95),
                    screens: None,
                },
                WorkspaceError::Ratio(0.95),
            ),
            (
                Operation::SetRatio { ratio: 0.1 },
                WorkspaceError::Ratio(0.1),
            ),
            (
                Operation::Untile {
                    keep: Some("c".into()),
                },
                WorkspaceError::NotVisible("c".into()),
            ),
        ] {
            assert_eq!(w.apply(&op, None, live), Err(expected));
            assert_eq!(w.snapshot(), before);
        }
        w.apply(
            &Operation::Single {
                session: "c".into(),
            },
            None,
            live,
        )
        .unwrap();
        assert_eq!(
            w.apply(&Operation::Swap, None, live),
            Err(WorkspaceError::NotSplit)
        );
        assert_eq!(
            w.apply(
                &Operation::FocusViewport {
                    viewport: Viewport::Second
                },
                None,
                live
            ),
            Err(WorkspaceError::NotSplit)
        );
    }

    #[test]
    fn stale_revision_is_refused() {
        let mut w = Workspace::default();
        let agent_read = w.apply(&tile("a", "b"), None, live).unwrap().revision;
        // A person drags in between.
        w.apply(&Operation::Swap, None, live).unwrap();
        let after_drag = w.snapshot();
        assert_eq!(
            w.apply(&tile("a", "b"), Some(agent_read), live),
            Err(WorkspaceError::Stale {
                expected: agent_read,
                current: agent_read + 1
            })
        );
        assert_eq!(w.snapshot(), after_drag);
    }

    #[test]
    fn removal_collapses_to_the_remaining_tile() {
        let mut w = Workspace::default();
        w.apply(
            &Operation::Tile {
                first: "a".into(),
                second: "b".into(),
                axis: Some(Axis::Vertical),
                ratio: None,
                screens: Some([ScreenLayout::Stacked, ScreenLayout::SideBySide]),
            },
            None,
            live,
        )
        .unwrap();
        w.apply(
            &Operation::FocusViewport {
                viewport: Viewport::Second,
            },
            None,
            live,
        )
        .unwrap();
        let revision = w.snapshot().revision;
        assert!(!w.remove("c"));
        assert_eq!(w.snapshot().revision, revision);
        assert!(w.remove("a"));
        let s = w.snapshot();
        assert_eq!(
            s.layout,
            Layout::Single {
                session: "b".into(),
                screens: ScreenLayout::SideBySide
            }
        );
        assert_eq!(s.focused_viewport, Viewport::First);
        assert!(w.remove("b"));
        assert_eq!(w.snapshot().layout, Layout::Empty);
    }

    #[test]
    fn tile_uses_defaults_then_keeps_current_split_settings() {
        let mut w = Workspace::default();
        w.set_defaults(SplitDefaults {
            axis: Axis::Vertical,
            ratio: 0.4,
        });
        w.apply(&tile("a", "b"), None, live).unwrap();
        w.apply(
            &Operation::SetAxis {
                axis: Axis::Horizontal,
            },
            None,
            live,
        )
        .unwrap();
        let s = w.apply(&tile("c", "a"), None, live).unwrap();
        let Layout::Split { axis, ratio, .. } = s.layout else {
            panic!()
        };
        assert_eq!((axis, ratio), (Axis::Horizontal, 0.4));
        w.set_defaults(SplitDefaults {
            axis: Axis::Auto,
            ratio: 3.0,
        });
        assert_eq!(w.defaults().ratio, RATIO_DEFAULT);
    }
}
