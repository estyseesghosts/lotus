//! Vector icons drawn for LOTUS (16 x 16 stroke SVG, no third-party art).
//!
//! GPUI rasterises an `svg()` element as an alpha mask tinted with the
//! element's text colour (`elements/svg.rs`), so stroke colour here is
//! irrelevant and every icon stays crisp at any display scale.
use gpui::{px, svg, IntoElement, Rgba, Styled};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Cpu,
    Threads,
    Breakpoints,
    Memory,
    Settings,
    Ipc,
    Logs,
    Gpu,
    Audio,
    Snapshots,
    Timeline,
    Result,
    More,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronDown,
    Play,
    Pause,
    Step,
    Reset,
    Load,
    Stop,
    Zoom,
    Stacked,
    SideBySide,
    Plus,
    Close,
    Gear,
}

macro_rules! svg16 {
    ($body:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">"#,
            $body,
            "</svg>"
        )
    };
}

impl Icon {
    #[must_use]
    pub fn svg(self) -> &'static str {
        match self {
            Self::Cpu => svg16!(
                r#"<rect x="4" y="4" width="8" height="8" rx="1"/><rect x="6.5" y="6.5" width="3" height="3"/><path d="M6 1.5v2M10 1.5v2M6 12.5v2M10 12.5v2M1.5 6h2M1.5 10h2M12.5 6h2M12.5 10h2"/>"#
            ),
            Self::Threads => svg16!(
                r#"<path d="M3 2.5v11M8 2.5v11M13 2.5v11"/><circle cx="3" cy="5" r="1.2" fill="black"/><circle cx="8" cy="10" r="1.2" fill="black"/><circle cx="13" cy="7" r="1.2" fill="black"/>"#
            ),
            Self::Breakpoints => svg16!(
                r#"<path d="M2 4h8.5L14 8l-3.5 4H2z"/><circle cx="6.5" cy="8" r="1.6" fill="black"/>"#
            ),
            Self::Memory => svg16!(
                r#"<rect x="1.5" y="4.5" width="13" height="7" rx="1"/><path d="M4 7v2M6.5 7v2M9 7v2M11.5 7v2M4 11.5v2M8 11.5v2M12 11.5v2"/>"#
            ),
            Self::Settings => svg16!(
                r#"<path d="M2 4h7M12 4h2M2 8h2M7 8h7M2 12h8M13 12h1"/><circle cx="10.5" cy="4" r="1.5"/><circle cx="5.5" cy="8" r="1.5"/><circle cx="11.5" cy="12" r="1.5"/>"#
            ),
            Self::Ipc => {
                svg16!(r#"<path d="M2 5.5h10M9.5 3l2.5 2.5L9.5 8M14 10.5H4M6.5 8L4 10.5 6.5 13"/>"#)
            }
            Self::Logs => svg16!(
                r#"<rect x="2.5" y="1.5" width="11" height="13" rx="1"/><path d="M5 5h6M5 8h6M5 11h4"/>"#
            ),
            Self::Gpu => svg16!(
                r#"<path d="M8 1.5l6 3.5v6l-6 3.5-6-3.5V5z"/><path d="M2 5l6 3.5L14 5M8 8.5v6"/>"#
            ),
            Self::Audio => svg16!(
                r#"<path d="M2.5 6h2.5L8.5 3v10L5 10H2.5z"/><path d="M11 5.5a3.5 3.5 0 0 1 0 5M12.8 3.5a6 6 0 0 1 0 9"/>"#
            ),
            Self::Snapshots => svg16!(
                r#"<path d="M2.5 8a5.5 5.5 0 1 0 1.6-3.9"/><path d="M2.5 2.5v2.5H5M8 5v3l2 1.5"/>"#
            ),
            Self::Timeline => svg16!(
                r#"<path d="M1.5 13.5h13"/><rect x="2.5" y="3" width="5" height="2.5" rx=".5"/><rect x="5.5" y="6.5" width="7" height="2.5" rx=".5"/><rect x="3.5" y="10" width="4" height="2" rx=".5"/>"#
            ),
            Self::Result => svg16!(r#"<path d="M2.5 4l3.5 4-3.5 4M8 12h5.5"/>"#),
            Self::More => svg16!(
                r#"<circle cx="3.5" cy="8" r="1" fill="black"/><circle cx="8" cy="8" r="1" fill="black"/><circle cx="12.5" cy="8" r="1" fill="black"/>"#
            ),
            Self::ChevronLeft => svg16!(r#"<path d="M10 3.5L5.5 8l4.5 4.5"/>"#),
            Self::ChevronRight => svg16!(r#"<path d="M6 3.5L10.5 8 6 12.5"/>"#),
            Self::ChevronUp => svg16!(r#"<path d="M3.5 10L8 5.5l4.5 4.5"/>"#),
            Self::ChevronDown => svg16!(r#"<path d="M3.5 6L8 10.5 12.5 6"/>"#),
            Self::Play => svg16!(r#"<path d="M4.5 2.5v11l9-5.5z" fill="black"/>"#),
            Self::Pause => svg16!(
                r#"<rect x="3.5" y="2.5" width="3" height="11" rx=".5" fill="black"/><rect x="9.5" y="2.5" width="3" height="11" rx=".5" fill="black"/>"#
            ),
            Self::Step => svg16!(r#"<path d="M3 3v10l7-5z" fill="black"/><path d="M13 3v10"/>"#),
            Self::Reset => svg16!(r#"<path d="M3 8a5 5 0 1 0 1.5-3.5"/><path d="M3 2.5V5h2.5"/>"#),
            Self::Load => svg16!(
                r#"<path d="M1.5 4.5V13h13V6H8L6.5 4.5z"/><path d="M8 11.5v-4M6 9.5l2-2 2 2"/>"#
            ),
            Self::Stop => {
                svg16!(r#"<rect x="3.5" y="3.5" width="9" height="9" rx="1" fill="black"/>"#)
            }
            Self::Zoom => svg16!(
                r#"<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5l3.5 3.5M5 7h4M7 5v4"/>"#
            ),
            Self::Stacked => svg16!(
                r#"<rect x="3" y="1.5" width="10" height="6" rx=".5"/><rect x="4" y="9" width="8" height="5.5" rx=".5"/>"#
            ),
            Self::SideBySide => svg16!(
                r#"<rect x="1" y="4" width="7.5" height="6" rx=".5"/><rect x="9.5" y="4.5" width="5.5" height="5" rx=".5"/>"#
            ),
            Self::Plus => svg16!(r#"<path d="M8 3v10M3 8h10"/>"#),
            Self::Close => svg16!(r#"<path d="M4 4l8 8M12 4l-8 8"/>"#),
            Self::Gear => svg16!(
                r#"<circle cx="8" cy="8" r="2.2"/><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"/>"#
            ),
        }
    }
}

/// The icon at `size` logical pixels in `colour`.
pub fn icon(icon: Icon, size: f32, colour: Rgba) -> impl IntoElement {
    svg()
        .data(icon.svg().as_bytes())
        .size(px(size))
        .flex_none()
        .text_color(colour)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_complete_svg_document() {
        use Icon::*;
        for icon in [
            Cpu,
            Threads,
            Breakpoints,
            Memory,
            Settings,
            Ipc,
            Logs,
            Gpu,
            Audio,
            Snapshots,
            Timeline,
            Result,
            More,
            ChevronLeft,
            ChevronRight,
            ChevronUp,
            ChevronDown,
            Play,
            Pause,
            Step,
            Reset,
            Load,
            Stop,
            Zoom,
            Stacked,
            SideBySide,
            Plus,
            Close,
            Gear,
        ] {
            let svg = icon.svg();
            assert!(
                svg.starts_with("<svg ") && svg.ends_with("</svg>"),
                "{icon:?}"
            );
            assert!(svg.contains(r#"viewBox="0 0 16 16""#), "{icon:?}");
        }
    }
}
