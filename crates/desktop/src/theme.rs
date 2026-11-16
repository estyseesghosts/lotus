//! Dark neutral palette after Vercel's Geist (see `../../vercel.md` in the
//! workspace): hierarchy from grey steps and borders, colour only for state.
use gpui::{rgb, Rgba};

/// Deepest background: the screen canvas.
pub fn background() -> Rgba {
    rgb(0x000000)
}
/// Sidebar and inspector panels.
pub fn panel() -> Rgba {
    rgb(0x1a1a1a)
}
/// Hover surface.
pub fn surface_hover() -> Rgba {
    rgb(0x1f1f1f)
}
/// Selected or pressed surface.
pub fn surface_active() -> Rgba {
    rgb(0x292929)
}
pub fn border() -> Rgba {
    rgb(0x2e2e2e)
}
pub fn border_strong() -> Rgba {
    rgb(0x454545)
}
pub fn text() -> Rgba {
    rgb(0xededed)
}
pub fn text_secondary() -> Rgba {
    rgb(0xa1a1a1)
}
pub fn text_muted() -> Rgba {
    rgb(0x878787)
}
pub fn info() -> Rgba {
    rgb(0x0095ff)
}
pub fn success() -> Rgba {
    rgb(0x279447)
}
pub fn warning() -> Rgba {
    rgb(0xd99327)
}
pub fn error() -> Rgba {
    rgb(0xe34646)
}

/// UI text face; Geist is not assumed to be installed.
pub const FONT: &str = "Segoe UI";
/// Figures, addresses and identifiers.
pub const MONO: &str = "Consolas";
