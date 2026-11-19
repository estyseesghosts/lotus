//! Opt-in Silvestris adapter (#138), staged on silvestris-adoption.
//! Geist Dark remains the default while the opt-in shell passes the gates in
//! docs/38-silvestris-adoption.md. Workers/MCP never depend on this module.
use gpui::App;

pub const STAGING_THEME: silvestris::ThemeId = silvestris::ThemeId::GEIST_DARK;

/// Register bundled fonts and install the shared theme before the first window.
pub fn initialize(initial_theme: Option<silvestris::ThemeId>, cx: &mut App) -> Result<(), String> {
    let store = crate::preferences::Store::load(crate::preferences::default_path());
    let theme = initial_theme.unwrap_or_else(|| store.ui.theme_id());
    silvestris::init(silvestris::ThemePreference::explicit(theme), cx)
        .map_err(|error| format!("{error:#}"))
}

pub fn parse_theme(value: &str) -> Option<silvestris::ThemeId> {
    match value {
        "aqua-light" => Some(silvestris::ThemeId::AQUA_LIGHT),
        "aqua-dark" => Some(silvestris::ThemeId::AQUA_DARK),
        "geist-light" => Some(silvestris::ThemeId::GEIST_LIGHT),
        "geist-dark" => Some(silvestris::ThemeId::GEIST_DARK),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn theme_arguments_and_staging_default_are_explicit() {
        let args = ["aqua-light", "aqua-dark", "geist-light", "geist-dark"];
        for (arg, expected) in args.into_iter().zip(silvestris::ThemeId::ALL) {
            assert_eq!(super::parse_theme(arg), Some(expected));
        }
        assert_eq!(super::parse_theme("unknown"), None);
        assert_eq!(
            crate::preferences::SharedUiPreferences::default().theme_id(),
            super::STAGING_THEME
        );
    }
    #[test]
    fn staging_palette_matches_legacy_surfaces_and_diagnostics() {
        let theme = crate::theme::legacy();
        assert_eq!(super::STAGING_THEME, silvestris::ThemeId::GEIST_DARK);
        let shared = silvestris::core::theme(super::STAGING_THEME);
        assert_eq!(theme.font, shared.type_.ui);
        assert_eq!(theme.mono, shared.type_.mono);
        for (new, old) in [
            (shared.colors.canvas, theme.canvas),
            (shared.colors.sidebar, theme.panel),
            (shared.colors.divider, theme.border),
            (shared.colors.text, theme.text),
            (shared.colors.text_secondary, theme.text_secondary),
            (shared.colors.info, theme.info),
            (shared.colors.success, theme.success),
            (shared.colors.warning, theme.warning),
            (shared.colors.error, theme.error),
        ] {
            assert_eq!(
                gpui::rgb((u32::from(new.r) << 16) | (u32::from(new.g) << 8) | u32::from(new.b)),
                old
            );
        }
    }
}
