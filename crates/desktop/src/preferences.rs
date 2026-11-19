//! Per-user desktop preferences: pane layout and opt-in integrations.
//!
//! Stored as JSON at `LOTUS_DESKTOP_PREFERENCES` if set, otherwise
//! `%APPDATA%\LOTUS\desktop.json` on Windows and
//! `$XDG_CONFIG_HOME/lotus/desktop.json` (or `~/.config/...`) elsewhere. The
//! portable launchers set the variable to keep the file inside the release
//! folder. Nothing here affects emulation profiles or guest state.
//!
//! A file that exists but does not parse is left untouched: the window uses
//! defaults and does not save over it.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::{INSPECTOR_DEFAULT, SESSION_DEFAULT};
use lotus_session::workspace::{Axis, RATIO_DEFAULT};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub sessions_open: bool,
    pub sessions_width: f32,
    pub inspector_open: bool,
    pub inspector_width: f32,
    /// Discord Rich Presence; off unless the user enables it.
    pub discord_presence: bool,
    /// Report "Playing a game" instead of file names.
    pub discord_hide_titles: bool,
    /// Overrides the built-in Discord application ID
    /// (`presence::DEFAULT_APPLICATION_ID`).
    pub discord_application_id: Option<String>,
    /// Split axis and first-tile ratio for a `tile` that leaves them out (lotus#67). The
    /// tiled sessions themselves are not saved: session IDs do not outlive the host, and
    /// restoring tiles must never start a worker.
    pub split_axis: Axis,
    pub split_ratio: f32,
    /// Settings window position and size `[x, y, width, height]` in logical pixels, saved
    /// when it closes (lotus#53); `None` centres it.
    pub settings_bounds: Option<[f32; 4]>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            sessions_open: true,
            sessions_width: SESSION_DEFAULT,
            inspector_open: false,
            inspector_width: INSPECTOR_DEFAULT,
            discord_presence: false,
            discord_hide_titles: false,
            discord_application_id: None,
            split_axis: Axis::Auto,
            split_ratio: RATIO_DEFAULT,
            settings_bounds: None,
        }
    }
}

/// Preferences and where they persist. `path` is `None` when there is no
/// location or the existing file is unreadable, which disables saving.
#[derive(Debug, Clone)]
pub struct Store {
    pub path: Option<PathBuf>,
    pub values: Preferences,
}

#[must_use]
pub fn default_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LOTUS_DESKTOP_PREFERENCES") {
        return Some(PathBuf::from(path));
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|dir| Path::new(&dir).join("LOTUS/desktop.json"));
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .map(|dir| dir.join("lotus/desktop.json"))
}

impl Store {
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> Self {
        let Some(path) = path else {
            return Self {
                path: None,
                values: Preferences::default(),
            };
        };
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(values) => Self {
                    path: Some(path),
                    values,
                },
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "ignoring unreadable desktop preferences");
                    Self {
                        path: None,
                        values: Preferences::default(),
                    }
                }
            },
            Err(_) => Self {
                path: Some(path),
                values: Preferences::default(),
            },
        }
    }

    /// Write through a temporary file and rename, so a crash never leaves a
    /// half-written file.
    pub fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let result = (|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, serde_json::to_vec_pretty(&self.values)?)?;
            std::fs::rename(&temporary, path)
        })();
        if let Err(error) = result {
            tracing::warn!(path = %path.display(), %error, "cannot save desktop preferences");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// lotus#50: `lotus.desktop.*` descriptors carry these defaults and the layout bounds.
    #[test]
    fn registry_matches_defaults_and_bounds() {
        use crate::layout::{INSPECTOR_MAX, INSPECTOR_MIN, SESSION_MAX, SESSION_MIN};
        use lotus_session::workspace::{RATIO_MAX, RATIO_MIN};
        use lotus_settings::{Kind, Owner, Registry};
        let defaults = serde_json::to_value(Preferences::default()).unwrap();
        let fields = defaults.as_object().unwrap();
        let desktop: Vec<_> = Registry::builtin()
            .descriptors()
            .iter()
            .filter(|d| d.owner == Owner::Desktop)
            .collect();
        assert_eq!(desktop.len(), fields.len());
        for d in desktop {
            let field = d.key.strip_prefix("lotus.desktop.").unwrap();
            assert_eq!(fields.get(field), Some(&d.default), "{}", d.key);
        }
        for (key, min, max) in [
            ("sessions_width", SESSION_MIN, SESSION_MAX),
            ("inspector_width", INSPECTOR_MIN, INSPECTOR_MAX),
            ("split_ratio", RATIO_MIN, RATIO_MAX),
        ] {
            let d = Registry::builtin()
                .get(&format!("lotus.desktop.{key}"))
                .unwrap();
            let Kind::Float {
                min: Some(low),
                max: Some(high),
            } = d.kind
            else {
                panic!("{key} is {:?}", d.kind)
            };
            // The window stores f32; compare at that precision.
            assert_eq!((low as f32, high as f32), (min, max), "{key}");
        }
    }

    #[test]
    fn round_trips_and_never_overwrites_an_unreadable_file() {
        let dir = std::env::temp_dir().join(format!("lotus-prefs-{}", std::process::id()));
        let path = dir.join("desktop.json");
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::load(Some(path.clone()));
        assert_eq!(store.values, Preferences::default());
        assert!(!store.values.discord_presence, "presence is opt-in");
        store.values.inspector_width = 512.0;
        store.save();
        assert_eq!(
            Store::load(Some(path.clone())).values.inspector_width,
            512.0
        );
        // Unknown and missing fields: defaults fill the gaps.
        std::fs::write(&path, br#"{"sessions_open":false,"future":1}"#).unwrap();
        let partial = Store::load(Some(path.clone())).values;
        assert!(!partial.sessions_open);
        assert_eq!(partial.inspector_width, INSPECTOR_DEFAULT);
        std::fs::write(&path, b"not json").unwrap();
        let broken = Store::load(Some(path.clone()));
        assert!(broken.path.is_none());
        broken.save();
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
