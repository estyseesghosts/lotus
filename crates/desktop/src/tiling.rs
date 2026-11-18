//! Drag-and-drop tiling (lotus#68): where a dragged session lands and the workspace
//! operation that commits it. Pure, so the snap preview the window draws and the operation
//! it sends come from the same [`Target`].
//!
//! The stage is cut along its diagonals into four edge zones; every point belongs to
//! exactly one, so there is no dead or ambiguous area. Rules:
//! - A sidebar row dropped on a single view tiles it on that edge beside the shown session
//!   (left/right → horizontal, top/bottom → vertical; the dragged session takes the edge).
//! - A sidebar row dropped on a split replaces the tile under the pointer; the displaced
//!   session keeps running in the sidebar.
//! - A tile header dragged onto a stage edge puts that session on that edge and the other
//!   tile opposite, which covers swap, reorder and orientation changes.
//! - A tile header dropped on the sidebar untiles it, keeping the other tile.
//!
//! Every operation carries the revision the drag started on, so a drop is refused if the
//! layout changed under it (an MCP edit, a stop) instead of acting on a stale preview.

use lotus_session::workspace::{Axis, Layout, Operation, Viewport, RATIO_MAX, RATIO_MIN};

/// Where the drag started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Row,
    Tile,
}

/// GPUI drag payload for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDrag {
    pub session: String,
    pub label: String,
    pub source: Source,
    /// Workspace revision when the drag started.
    pub revision: u64,
}

/// GPUI drag payload for the split divider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DividerDrag;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Edge(Edge),
    Tile(Viewport),
    Sidebar,
}

/// The edge zone holding `(x, y)` in a `width` x `height` area, split by its diagonals.
#[must_use]
pub fn edge_at(x: f32, y: f32, width: f32, height: f32) -> Edge {
    let (u, v) = (x / width.max(1.0) - 0.5, y / height.max(1.0) - 0.5);
    if u.abs() >= v.abs() {
        if u < 0.0 {
            Edge::Left
        } else {
            Edge::Right
        }
    } else if v < 0.0 {
        Edge::Top
    } else {
        Edge::Bottom
    }
}

/// Position of the split between the two tiles along an axis of length `size` starting at
/// 0: the stage pads by `padding` on both sides and puts `gap` between the slots, which
/// share what is left by `ratio` (the layout `split_plan` draws).
#[must_use]
pub fn split_point(size: f32, padding: f32, gap: f32, ratio: f32) -> f32 {
    let along = (size - 2.0 * padding - gap).max(0.0);
    padding + along * ratio + gap / 2.0
}

/// Inverse of [`split_point`]: the ratio that puts the split at `pos`, within the
/// workspace bounds.
#[must_use]
pub fn ratio_at(pos: f32, size: f32, padding: f32, gap: f32) -> f32 {
    let along = (size - 2.0 * padding - gap).max(1.0);
    ((pos - padding - gap / 2.0) / along).clamp(RATIO_MIN, RATIO_MAX)
}

/// Target for a drag at `(x, y)` inside the stage. `vertical` and `ratio` describe the
/// split as drawn. `None` where the drop would change nothing.
#[must_use]
pub fn stage_target(
    drag: &SessionDrag,
    layout: &Layout,
    (x, y, width, height): (f32, f32, f32, f32),
    vertical: bool,
    ratio: f32,
    gap: (f32, f32),
) -> Option<Target> {
    let target = match (layout, drag.source) {
        (Layout::Split { .. }, Source::Row)
            if !layout.visible().contains(&drag.session.as_str()) =>
        {
            let (pos, size) = if vertical { (y, height) } else { (x, width) };
            Target::Tile(if pos < split_point(size, gap.0, gap.1, ratio) {
                Viewport::First
            } else {
                Viewport::Second
            })
        }
        _ => Target::Edge(edge_at(x, y, width, height)),
    };
    operation(drag, target, layout, vertical).map(|_| target)
}

/// The workspace operation for dropping `drag` on `target`; `None` when it is not a valid
/// drop or would change nothing. `vertical` is the split orientation as drawn.
#[must_use]
pub fn operation(
    drag: &SessionDrag,
    target: Target,
    layout: &Layout,
    vertical: bool,
) -> Option<Operation> {
    let dragged = drag.session.clone();
    match (layout, drag.source, target) {
        (Layout::Single { session, .. }, Source::Row, Target::Edge(edge))
            if *session != dragged =>
        {
            Some(edge_tile(edge, dragged, session.clone()))
        }
        (Layout::Empty, Source::Row, Target::Edge(_)) => {
            Some(Operation::Single { session: dragged })
        }
        (Layout::Split { first, second, .. }, Source::Row, Target::Tile(viewport))
            if *first != dragged && *second != dragged =>
        {
            let (first, second) = match viewport {
                Viewport::First => (dragged, second.clone()),
                Viewport::Second => (first.clone(), dragged),
            };
            Some(Operation::Tile {
                first,
                second,
                axis: None,
                ratio: None,
                screens: None,
            })
        }
        (Layout::Split { first, second, .. }, _, Target::Edge(edge)) => {
            let other = if *first == dragged {
                second.clone()
            } else if *second == dragged {
                first.clone()
            } else {
                return None;
            };
            let Operation::Tile {
                first: a,
                second: b,
                axis: to,
                ..
            } = edge_tile(edge, dragged, other)
            else {
                return None;
            };
            // Dropping where it already is, as drawn, changes nothing (and keeps `auto`).
            let drawn = if vertical {
                Axis::Vertical
            } else {
                Axis::Horizontal
            };
            let unchanged = to == Some(drawn) && a == *first && b == *second;
            (!unchanged).then_some(Operation::Tile {
                first: a,
                second: b,
                axis: to,
                ratio: None,
                screens: None,
            })
        }
        (Layout::Split { first, second, .. }, Source::Tile, Target::Sidebar) => {
            let keep = if *first == dragged {
                second.clone()
            } else if *second == dragged {
                first.clone()
            } else {
                return None;
            };
            Some(Operation::Untile { keep: Some(keep) })
        }
        _ => None,
    }
}

/// `dragged` on `edge`, `other` opposite.
fn edge_tile(edge: Edge, dragged: String, other: String) -> Operation {
    let (first, second, axis) = match edge {
        Edge::Left => (dragged, other, Axis::Horizontal),
        Edge::Right => (other, dragged, Axis::Horizontal),
        Edge::Top => (dragged, other, Axis::Vertical),
        Edge::Bottom => (other, dragged, Axis::Vertical),
    };
    Operation::Tile {
        first,
        second,
        axis: Some(axis),
        ratio: None,
        screens: None,
    }
}

/// Snap preview for `target` as fractions `(x, y, w, h)` of the stage. `split` is where
/// the tiles meet as a fraction of the stage ([`split_point`] / size), the same boundary
/// [`stage_target`] tests against.
#[must_use]
pub fn preview(target: Target, vertical: bool, split: f32) -> (f32, f32, f32, f32) {
    match target {
        Target::Edge(Edge::Left) => (0.0, 0.0, 0.5, 1.0),
        Target::Edge(Edge::Right) => (0.5, 0.0, 0.5, 1.0),
        Target::Edge(Edge::Top) => (0.0, 0.0, 1.0, 0.5),
        Target::Edge(Edge::Bottom) => (0.0, 0.5, 1.0, 0.5),
        Target::Tile(viewport) => {
            let (start, len) = match viewport {
                Viewport::First => (0.0, split),
                Viewport::Second => (split, 1.0 - split),
            };
            if vertical {
                (0.0, start, 1.0, len)
            } else {
                (start, 0.0, len, 1.0)
            }
        }
        Target::Sidebar => (0.0, 0.0, 1.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lotus_session::workspace::ScreenLayout;

    fn drag(session: &str, source: Source) -> SessionDrag {
        SessionDrag {
            session: session.into(),
            label: session.into(),
            source,
            revision: 1,
        }
    }

    fn single(session: &str) -> Layout {
        Layout::Single {
            session: session.into(),
            screens: ScreenLayout::Auto,
        }
    }

    fn split(first: &str, second: &str, axis: Axis) -> Layout {
        Layout::Split {
            first: first.into(),
            second: second.into(),
            axis,
            ratio: 0.5,
            screens: [ScreenLayout::Auto; 2],
        }
    }

    fn tile(first: &str, second: &str, axis: Option<Axis>) -> Option<Operation> {
        Some(Operation::Tile {
            first: first.into(),
            second: second.into(),
            axis,
            ratio: None,
            screens: None,
        })
    }

    #[test]
    fn every_stage_point_has_exactly_the_edge_its_preview_shows() {
        let (w, h) = (1200.0, 700.0);
        for i in 0..=48 {
            for j in 0..=28 {
                let (x, y) = (w * i as f32 / 48.0, h * j as f32 / 28.0);
                let edge = edge_at(x, y, w, h);
                let (px, py, pw, ph) = preview(Target::Edge(edge), false, 0.5);
                // The pointer is always inside the half its preview highlights.
                assert!(
                    x >= px * w - 0.01 && x <= (px + pw) * w + 0.01,
                    "{x},{y} {edge:?}"
                );
                assert!(
                    y >= py * h - 0.01 && y <= (py + ph) * h + 0.01,
                    "{x},{y} {edge:?}"
                );
            }
        }
        assert_eq!(edge_at(10.0, 350.0, w, h), Edge::Left);
        assert_eq!(edge_at(1190.0, 350.0, w, h), Edge::Right);
        assert_eq!(edge_at(600.0, 10.0, w, h), Edge::Top);
        assert_eq!(edge_at(600.0, 690.0, w, h), Edge::Bottom);
    }

    #[test]
    fn row_onto_single_view_tiles_on_the_dropped_edge() {
        let b = drag("b", Source::Row);
        let layout = single("a");
        let horizontal = Some(Axis::Horizontal);
        let vertical = Some(Axis::Vertical);
        assert_eq!(
            operation(&b, Target::Edge(Edge::Left), &layout, false),
            tile("b", "a", horizontal)
        );
        assert_eq!(
            operation(&b, Target::Edge(Edge::Right), &layout, false),
            tile("a", "b", horizontal)
        );
        assert_eq!(
            operation(&b, Target::Edge(Edge::Top), &layout, false),
            tile("b", "a", vertical)
        );
        assert_eq!(
            operation(&b, Target::Edge(Edge::Bottom), &layout, false),
            tile("a", "b", vertical)
        );
        // The shown session onto itself, or a row onto the sidebar, does nothing.
        assert_eq!(
            operation(
                &drag("a", Source::Row),
                Target::Edge(Edge::Left),
                &layout,
                false
            ),
            None
        );
        assert_eq!(operation(&b, Target::Sidebar, &layout, false), None);
        assert_eq!(
            operation(&b, Target::Edge(Edge::Left), &Layout::Empty, false),
            Some(Operation::Single {
                session: "b".into()
            })
        );
    }

    #[test]
    fn row_onto_split_replaces_only_the_tile_under_the_pointer() {
        let c = drag("c", Source::Row);
        let layout = split("a", "b", Axis::Horizontal);
        let area = (1200.0, 700.0);
        let at = |x: f32| {
            stage_target(
                &c,
                &layout,
                (x, 350.0, area.0, area.1),
                false,
                0.5,
                (24.0, 12.0),
            )
        };
        assert_eq!(at(100.0), Some(Target::Tile(Viewport::First)));
        assert_eq!(at(1100.0), Some(Target::Tile(Viewport::Second)));
        assert_eq!(
            operation(&c, Target::Tile(Viewport::First), &layout, false),
            tile("c", "b", None)
        );
        assert_eq!(
            operation(&c, Target::Tile(Viewport::Second), &layout, false),
            tile("a", "c", None)
        );
        // A row already tiled behaves like its tile.
        assert_eq!(
            stage_target(
                &drag("a", Source::Row),
                &layout,
                (1100.0, 350.0, area.0, area.1),
                false,
                0.5,
                (24.0, 12.0)
            ),
            Some(Target::Edge(Edge::Right))
        );
    }

    #[test]
    fn tile_drags_swap_reorder_and_reorient_without_no_op_commits() {
        let a = drag("a", Source::Tile);
        let layout = split("a", "b", Axis::Auto);
        // Already left in a horizontal layout: nothing to do, `auto` is kept.
        assert_eq!(
            operation(&a, Target::Edge(Edge::Left), &layout, false),
            None
        );
        assert_eq!(
            operation(&a, Target::Edge(Edge::Right), &layout, false),
            tile("b", "a", Some(Axis::Horizontal))
        );
        assert_eq!(
            operation(&a, Target::Edge(Edge::Top), &layout, false),
            tile("a", "b", Some(Axis::Vertical))
        );
        assert_eq!(
            operation(&a, Target::Edge(Edge::Bottom), &layout, true),
            tile("b", "a", Some(Axis::Vertical))
        );
        // Drawn vertical: the top edge is where it is.
        assert_eq!(operation(&a, Target::Edge(Edge::Top), &layout, true), None);
        assert_eq!(
            operation(&a, Target::Sidebar, &layout, false),
            Some(Operation::Untile {
                keep: Some("b".into())
            })
        );
        // A tile from a layout that has since changed is not in it any more.
        assert_eq!(
            operation(&drag("z", Source::Tile), Target::Sidebar, &layout, false),
            None
        );
    }

    #[test]
    fn divider_ratio_round_trips_and_stays_in_bounds() {
        for ratio in [0.2, 0.35, 0.5, 0.65, 0.8] {
            let at = split_point(1400.0, 24.0, 12.0, ratio);
            assert!((ratio_at(at, 1400.0, 24.0, 12.0) - ratio).abs() < 1e-4);
        }
        assert_eq!(ratio_at(0.0, 1400.0, 24.0, 12.0), RATIO_MIN);
        assert_eq!(ratio_at(1400.0, 1400.0, 24.0, 12.0), RATIO_MAX);
    }
}
