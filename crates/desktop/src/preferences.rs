//! Per-user desktop preferences: pane layout and opt-in integrations.
//!
//! Stored as JSON at `LOTUS_DESKTOP_PREFERENCES` if set, otherwise
//! `%APPDATA%\LOTUS\desktop.json` on Windows and
//! `$XDG_CONFIG_HOME/lotus/desktop.json` (or `~/.config/...`) elsewhere. The
//! portable launchers set the variable to keep the file inside the release
//! folder. Nothing here affects emulation profiles or guest state.
//!
//! A file that exists but does not parse is left untouched: the window uses
//! defaults and does not save over it. Read failures other than a missing file
//! also disable saving. Unknown fields survive saves during staged UI adoption
//! (handbook 19, "Preference migration strategy").
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
    // Keep outside Preferences: these are not editable registry descriptors.
    // Known fields always come from the current typed values when saving.
    original: serde_json::Map<String, serde_json::Value>,
    #[cfg(feature = "silvestris-ui")]
    pub ui: SharedUiPreferences,
}

/// Staged shell metadata, separate from the existing editable registry keys.
/// Width/visibility still use the legacy keys so rollback keeps current layout.
#[cfg(feature = "silvestris-ui")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedUiPreferences {
    pub schema_version: u32,
    pub inspector_pinned: bool,
    pub inspector_panel: String,
    pub theme_family: silvestris::ThemeFamily,
    pub appearance: silvestris::Appearance,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[cfg(feature = "silvestris-ui")]
impl Default for SharedUiPreferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            inspector_pinned: false,
            inspector_panel: "cpu".into(),
            theme_family: silvestris::ThemeId::AQUA_LIGHT.family,
            appearance: silvestris::ThemeId::AQUA_LIGHT.appearance,
            extra: Default::default(),
        }
    }
}

#[cfg(feature = "silvestris-ui")]
impl SharedUiPreferences {
    pub fn theme_id(&self) -> silvestris::ThemeId {
        silvestris::ThemeId::new(self.theme_family, self.appearance)
    }
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
                original: Default::default(),
                #[cfg(feature = "silvestris-ui")]
                ui: Default::default(),
            };
        };
        match std::fs::read(&path) {
            Ok(bytes) => match Self::from_bytes(&path, &bytes) {
                Ok(store) => store,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "ignoring unreadable desktop preferences");
                    Self {
                        path: None,
                        values: Preferences::default(),
                        original: Default::default(),
                        #[cfg(feature = "silvestris-ui")]
                        ui: Default::default(),
                    }
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self {
                path: Some(path),
                values: Preferences::default(),
                original: Default::default(),
                #[cfg(feature = "silvestris-ui")]
                ui: Default::default(),
            },
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "cannot read desktop preferences; saving disabled");
                Self {
                    path: None,
                    values: Preferences::default(),
                    original: Default::default(),
                    #[cfg(feature = "silvestris-ui")]
                    ui: Default::default(),
                }
            }
        }
    }

    fn from_bytes(path: &Path, bytes: &[u8]) -> serde_json::Result<Self> {
        let (values, original) = Self::decode(bytes)?;
        #[cfg(feature = "silvestris-ui")]
        let ui = if let Some(ui) = original.get("silvestris") {
            let ui: SharedUiPreferences = serde_json::from_value(ui.clone())?;
            if ui.schema_version != 1 {
                return Err(serde_json::Error::io(std::io::Error::other(
                    "unsupported Silvestris preferences schema",
                )));
            }
            ui
        } else {
            // Handbook 14/19: preserve existing docked intent. New files
            // default to overlay; only an explicitly saved open pane pins.
            SharedUiPreferences {
                inspector_pinned: original.get("inspector_open") == Some(&true.into()),
                ..Default::default()
            }
        };
        Ok(Self {
            path: Some(path.to_path_buf()),
            values,
            original,
            #[cfg(feature = "silvestris-ui")]
            ui,
        })
    }

    fn decode(
        bytes: &[u8],
    ) -> serde_json::Result<(Preferences, serde_json::Map<String, serde_json::Value>)> {
        let original = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(bytes)?;
        let values = serde_json::from_value(serde_json::Value::Object(original.clone()))?;
        Ok((values, original))
    }

    /// Write through a temporary file and rename, so a crash never leaves a
    /// half-written file.
    pub fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let result = (|| {
            let mut document = self.original.clone();
            #[cfg(feature = "silvestris-ui")]
            document.insert("silvestris".into(), serde_json::to_value(&self.ui)?);
            if let serde_json::Value::Object(known) = serde_json::to_value(&self.values)? {
                document.extend(known);
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, serde_json::to_vec_pretty(&document)?)?;
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

    fn test_directory(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("lotus-prefs-{}-{name}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[cfg(feature = "silvestris-ui")]
    #[test]
    fn shell_migration_preserves_saved_dock_intent_and_legacy_layout() {
        let dir = test_directory("shell");
        let path = dir.join("desktop.json");
        assert!(!Store::load(Some(path.clone())).ui.inspector_pinned);
        for open in [false, true] {
            let old = serde_json::json!({"sessions_open": false, "sessions_width": 333.,
                "inspector_open": open, "inspector_width": 512.,
                "settings_bounds": [1., 2., 900., 700.]});
            std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
            let mut store = Store::load(Some(path.clone()));
            assert_eq!(store.ui.inspector_pinned, open);
            assert_eq!(store.ui.inspector_panel, "cpu");
            store.ui.inspector_panel = "gpu".into();
            store
                .ui
                .extra
                .insert("future".into(), serde_json::json!({"a": [1, 2]}));
            store.save();
            let bytes = std::fs::read(&path).unwrap();
            let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            for (key, value) in old.as_object().unwrap() {
                assert_eq!(document.get(key), Some(value), "{key}");
            }
            let loaded = Store::load(Some(path.clone()));
            assert_eq!(loaded.ui.inspector_pinned, open);
            assert_eq!(loaded.ui.inspector_panel, "gpu");
            assert_eq!(loaded.ui.extra, store.ui.extra);
            loaded.save();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[cfg(feature = "silvestris-ui")]
    #[test]
    fn aqua_light_fills_only_unsaved_theme_dimensions() {
        use silvestris::ThemeId;

        let dir = test_directory("theme-default");
        let path = dir.join("desktop.json");
        assert_eq!(
            Store::load(Some(path.clone())).ui.theme_id(),
            ThemeId::AQUA_LIGHT
        );
        for (saved, expected) in [
            (r#"{"inspector_open":true}"#, ThemeId::AQUA_LIGHT),
            (
                r#"{"silvestris":{"theme_family":"Geist"}}"#,
                ThemeId::GEIST_LIGHT,
            ),
            (
                r#"{"silvestris":{"appearance":"Dark"}}"#,
                ThemeId::AQUA_DARK,
            ),
            (
                r#"{"silvestris":{"theme_family":"Geist","appearance":"Dark"}}"#,
                ThemeId::GEIST_DARK,
            ),
        ] {
            std::fs::write(&path, saved).unwrap();
            let store = Store::load(Some(path.clone()));
            assert!(store.path.is_some(), "{saved}");
            assert_eq!(store.ui.theme_id(), expected);
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[cfg(feature = "silvestris-ui")]
    #[test]
    fn theme_dimensions_round_trip_without_changing_layout_or_future_fields() {
        let dir = test_directory("themes");
        let path = dir.join("desktop.json");
        std::fs::write(
            &path,
            br#"{"sessions_width":333,"inspector_width":511,"silvestris":{"future":[1,2]}}"#,
        )
        .unwrap();
        for id in silvestris::ThemeId::ALL {
            let mut store = Store::load(Some(path.clone()));
            store.ui.theme_family = id.family;
            store.ui.appearance = id.appearance;
            store.save();
            let loaded = Store::load(Some(path.clone()));
            assert_eq!(loaded.ui.theme_id(), id);
            assert_eq!(loaded.ui.extra["future"], serde_json::json!([1, 2]));
            assert_eq!(
                (loaded.values.sessions_width, loaded.values.inspector_width),
                (333., 511.)
            );
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[cfg(feature = "silvestris-ui")]
    #[test]
    fn unsupported_or_malformed_shell_metadata_is_preserved() {
        let dir = test_directory("shell-invalid");
        let path = dir.join("desktop.json");
        for text in [
            r#"{"silvestris":{"schema_version":2,"future":true}}"#,
            r#"{"silvestris":{"inspector_pinned":"bad"}}"#,
            r#"{"silvestris":null}"#,
            r#"{"silvestris":{"theme_family":"future"}}"#,
            r#"{"silvestris":{"appearance":"future"}}"#,
        ] {
            std::fs::write(&path, text).unwrap();
            let store = Store::load(Some(path.clone()));
            assert!(store.path.is_none());
            store.save();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn saves_known_edits_without_losing_future_ui_fields() {
        let dir = test_directory("forward");
        let path = dir.join("desktop.json");
        let original = serde_json::json!({
            "sessions_open": false,
            "sessions_width": 333.0,
            "inspector_width": 512.0,
            "settings_bounds": [40.0, 60.0, 900.0, 700.0],
            "schema_version": 7,
            "appearance": {"family": "future", "extra": [1, null, {"a": true}]},
            "future_null": null
        });
        std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        let mut store = Store::load(Some(path.clone()));
        assert!(store.path.is_some());
        assert_eq!(store.values.sessions_width, 333.0);
        // Settings edits reconstruct the typed object; extras must survive that path.
        let mut edited = serde_json::to_value(&store.values).unwrap();
        edited["sessions_open"] = true.into();
        store.values = serde_json::from_value(edited).unwrap();
        store.save();
        let first = std::fs::read(&path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(saved["sessions_open"], true);
        for key in [
            "sessions_width",
            "inspector_width",
            "settings_bounds",
            "schema_version",
            "appearance",
            "future_null",
        ] {
            assert_eq!(saved.get(key), original.get(key), "{key}");
        }
        Store::load(Some(path.clone())).save();
        assert_eq!(std::fs::read(&path).unwrap(), first, "idempotent save");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn invalid_known_values_and_non_objects_are_never_overwritten() {
        let dir = test_directory("invalid");
        let path = dir.join("desktop.json");
        for bytes in [
            b"null".as_slice(),
            b"[]".as_slice(),
            br#"{"sessions_width":"bad","future":true}"#.as_slice(),
        ] {
            std::fs::write(&path, bytes).unwrap();
            let store = Store::load(Some(path.clone()));
            assert!(store.path.is_none());
            store.save();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn io_read_failures_disable_saving() {
        let dir = test_directory("io-error");
        // Reading a directory as a file fails on Windows and Unix. It must not
        // be treated as a new preferences file, even if the path later changes.
        let store = Store::load(Some(dir.clone()));
        assert!(store.path.is_none());
        std::fs::remove_dir(&dir).unwrap();
        store.save();
        assert!(!dir.exists());
        assert!(!dir.with_extension("json.tmp").exists());
    }

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
