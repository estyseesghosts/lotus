//! Lotus-owned semantic palette adapter. Resolved per render from Silvestris
//! tokens (#121/#126/#138); default builds retain the original Geist palette.
use gpui::{rgb, App, Rgba};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub canvas: Rgba,
    pub window: Rgba,
    pub panel: Rgba,
    pub surface_hover: Rgba,
    pub surface_active: Rgba,
    pub border: Rgba,
    pub border_strong: Rgba,
    pub text: Rgba,
    pub text_secondary: Rgba,
    pub text_muted: Rgba,
    pub info: Rgba,
    pub success: Rgba,
    pub warning: Rgba,
    pub error: Rgba,
    pub primary_fill: Rgba,
    pub primary_text: Rgba,
    pub ui_size: f32,
    pub font: &'static str,
    pub mono: &'static str,
}

/// Pre-adoption colours, for the rollback renderer and baseline comparisons.
pub fn legacy() -> Palette {
    Palette {
        canvas: rgb(0x000000),
        window: rgb(0x000000),
        panel: rgb(0x1a1a1a),
        surface_hover: rgb(0x1f1f1f),
        surface_active: rgb(0x292929),
        border: rgb(0x2e2e2e),
        border_strong: rgb(0x454545),
        text: rgb(0xededed),
        text_secondary: rgb(0xa1a1a1),
        text_muted: rgb(0x878787),
        info: rgb(0x0095ff),
        success: rgb(0x279447),
        warning: rgb(0xd99327),
        error: rgb(0xe34646),
        primary_fill: rgb(0xededed),
        primary_text: rgb(0x000000),
        ui_size: 14.,
        #[cfg(not(feature = "silvestris-ui"))]
        font: "Segoe UI",
        #[cfg(feature = "silvestris-ui")]
        font: silvestris::core::fonts::INTER,
        #[cfg(not(feature = "silvestris-ui"))]
        mono: "Consolas",
        #[cfg(feature = "silvestris-ui")]
        mono: silvestris::core::fonts::GEIST_MONO,
    }
}

pub fn palette(cx: &App) -> Palette {
    #[cfg(not(feature = "silvestris-ui"))]
    {
        let _ = cx;
        legacy()
    }
    #[cfg(feature = "silvestris-ui")]
    {
        use silvestris::ActiveTheme;
        from_theme(cx.theme())
    }
}

#[cfg(feature = "silvestris-ui")]
pub fn from_theme(theme: &silvestris::core::Theme) -> Palette {
    fn rgba(c: silvestris::core::color::Rgba8) -> Rgba {
        gpui::rgba(
            (u32::from(c.r) << 24)
                | (u32::from(c.g) << 16)
                | (u32::from(c.b) << 8)
                | u32::from(c.a),
        )
    }
    let colors = &theme.colors;
    Palette {
        canvas: rgba(colors.canvas),
        window: rgba(colors.window),
        panel: rgba(colors.sidebar),
        surface_hover: rgba(theme.controls.push.normal.upper.from),
        surface_active: rgba(theme.controls.push.pressed.upper.from),
        border: rgba(colors.divider),
        // A structural border is neutral; Aqua's pressed button edge is blue.
        border_strong: rgba(theme.controls.field.border),
        text: rgba(colors.text),
        text_secondary: rgba(colors.text_secondary),
        // Explanatory text is not disabled UI: retain readable secondary contrast.
        text_muted: rgba(colors.text_secondary),
        info: rgba(colors.info),
        success: rgba(colors.success),
        warning: rgba(colors.warning),
        error: rgba(colors.error),
        // Legacy flat actions need the body under the glyph, not the glossy
        // upper highlight (white text on Dark Aqua's highlight lacks contrast).
        primary_fill: rgba(theme.controls.push_default.lower.from),
        primary_text: rgba(theme.controls.push_default.text),
        ui_size: theme.type_.size_regular,
        font: theme.type_.ui,
        mono: theme.type_.mono,
    }
}

#[cfg(all(test, feature = "silvestris-ui"))]
mod tests {
    use super::*;
    fn luminance(c: Rgba) -> f32 {
        let linear = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
    }
    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn light_ui_text_never_inherits_the_dark_preview_canvas() {
        for id in silvestris::ThemeId::ALL {
            let theme = silvestris::core::theme(id);
            let palette = from_theme(&theme);
            for background in [palette.window, palette.panel] {
                assert!(contrast(palette.text, background) >= 4.5, "{id:?}");
                assert!(contrast(palette.text_muted, background) >= 4.5, "{id:?}");
            }
            assert!(
                contrast(palette.primary_text, palette.primary_fill) >= 4.5,
                "{id:?}"
            );
            assert_eq!(
                (palette.font, palette.mono),
                (theme.type_.ui, theme.type_.mono)
            );
        }
    }
}
