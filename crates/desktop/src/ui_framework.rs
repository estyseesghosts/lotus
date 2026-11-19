//! Opt-in Silvestris adapter (#138), staged on silvestris-adoption.
//! Retain Geist Dark while the opt-in shell passes the migration gates in
//! docs/38-silvestris-adoption.md pass. Workers/MCP never depend on this module.
use gpui::App;

pub const STAGING_THEME: silvestris::ThemeId = silvestris::ThemeId::GEIST_DARK;

/// Register bundled fonts and install the shared theme before the first window.
pub fn initialize(cx: &mut App) -> Result<(), String> {
    silvestris::init(silvestris::ThemePreference::explicit(STAGING_THEME), cx)
        .map_err(|error| format!("{error:#}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn staging_palette_matches_legacy_surfaces_and_diagnostics() {
        assert_eq!(super::STAGING_THEME, silvestris::ThemeId::GEIST_DARK);
        let shared = silvestris::core::theme(super::STAGING_THEME);
        assert_eq!(crate::theme::FONT, shared.type_.ui);
        assert_eq!(crate::theme::MONO, shared.type_.mono);
        for (new, old) in [
            (shared.colors.canvas, crate::theme::background()),
            (shared.colors.sidebar, crate::theme::panel()),
            (shared.colors.divider, crate::theme::border()),
            (shared.colors.text, crate::theme::text()),
            (shared.colors.text_secondary, crate::theme::text_secondary()),
            (shared.colors.info, crate::theme::info()),
            (shared.colors.success, crate::theme::success()),
            (shared.colors.warning, crate::theme::warning()),
            (shared.colors.error, crate::theme::error()),
        ] {
            assert_eq!(
                gpui::rgb((u32::from(new.r) << 16) | (u32::from(new.g) << 8) | u32::from(new.b)),
                old
            );
        }
    }
}
