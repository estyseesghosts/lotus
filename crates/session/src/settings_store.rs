//! Layered worker settings (lotus#51) and session input layouts (lotus#63).
//!
//! Resolution, lowest to highest:
//! 1. compiled defaults: the registry default, which is the worker's own default
//!    (`crates/settings/azahar-inventory.json`);
//! 2. saved global defaults: `<storage root>/settings/global.json`;
//! 3. saved primary-card overrides: `cards/<card>/lotus-settings.json`, beside the card's
//!    `sdmc/`, `nand/` and `sysdata/` but never inside them, so a secondary's guest copy
//!    ([`crate::card_store::GUEST_DIRS`]) never carries it;
//! 4. a secondary's runtime overrides, kept in memory by the supervisor only.
//!
//! Input-layout keys (`lotus.input.*`, [`Owner::Input`]) use the same files and layers but
//! never reach a worker: [`Resolution::profile`] leaves them out and [`Resolution::input`]
//! turns them into the session's layout.
//!
//! The host configuration `profile` is applied above all of these and cannot be edited here:
//! it is managed by the host JSON or command line, and a saved entry it shadows is reported
//! as a conflict, not removed.
//!
//! Files hold only explicit entries (`{"version": 1, "values": {key: value}}`). Removing an
//! entry resumes inheritance. Entries this build cannot use (unknown, excluded or invalid
//! keys, for example written by a newer Lotus) are reported and ignored, and kept on write.
//! A file that does not parse, or has another version, is reported and never written.
//! Every write holds `settings/settings.lock` (an OS file lock, so other Lotus processes on
//! the same storage root wait), re-reads the file, changes one entry and renames a complete
//! temporary file over it, so concurrent writers never drop each other's entries.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lotus_input::layout::{self, Preset};
use lotus_protocol::debug::SettingValue;
use lotus_settings::{Availability, Branch, Owner, Registry, Scope};
use serde::Serialize;
use serde_json::{Map, Value};

/// Version of the global and card settings files.
pub const FILE_VERSION: u64 = 1;
/// Card overrides, relative to the card root.
pub const CARD_FILE: &str = "lotus-settings.json";

/// Where an effective value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Default,
    Global,
    Card,
    Runtime,
    /// The worker reported a value different from the saved or runtime profile.
    Worker,
    /// Host configuration `profile`; read-only here.
    Managed,
}

/// A saved settings file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Store {
    Global,
    Card(String),
}

impl Store {
    fn layer(&self) -> Layer {
        match self {
            Store::Global => Layer::Global,
            Store::Card(_) => Layer::Card,
        }
    }
    fn scope(&self) -> Scope {
        match self {
            Store::Global => Scope::Global,
            Store::Card(_) => Scope::Primary,
        }
    }
}

/// One setting's effective value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub value: Value,
    pub source: Layer,
    /// Lower layers that set the key and are hidden by `source`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shadowed: Vec<Layer>,
}

/// An entry or file that resolution could not use.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Problem {
    pub layer: Layer,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub reason: String,
}

/// Every available worker setting with its effective value and source.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Resolution {
    pub settings: BTreeMap<String, Entry>,
    pub problems: Vec<Problem>,
    /// Saved files that are not written because they do not parse or have another version.
    pub read_only: Vec<PathBuf>,
}

impl Resolution {
    /// Entries a worker must be sent: worker settings not at their compiled default source.
    #[must_use]
    pub fn profile(&self) -> BTreeMap<String, SettingValue> {
        self.settings
            .iter()
            .filter(|(_, entry)| entry.source != Layer::Default)
            .filter(|(key, _)| owner(key) == Some(Owner::Azahar))
            .filter_map(|(key, entry)| {
                serde_json::from_value(entry.value.clone())
                    .ok()
                    .map(|value| (key.clone(), value))
            })
            .collect()
    }

    /// The input layout these settings select: preset name, per-action overrides with their
    /// layer, stick and threshold profile. Values were validated when loaded.
    #[must_use]
    pub fn input(&self) -> InputSettings {
        use lotus_input::layout::{Action, Layer as InputLayer, StickProfile};
        let get = |key: &str| self.settings.get(key).map(|entry| &entry.value);
        let float = |key: &str, default: f32| {
            get(key)
                .and_then(Value::as_f64)
                .map_or(default, |v| v as f32)
        };
        let mut overrides = layout::Overrides::new();
        let mut problems = Vec::new();
        for action in Action::ALL {
            let Some(entry) = self
                .settings
                .get(&format!("lotus.input.bind.{}", action.id()))
            else {
                continue;
            };
            let layer = match entry.source {
                Layer::Default => continue,
                Layer::Global => InputLayer::Global,
                Layer::Card => InputLayer::Card,
                Layer::Runtime => InputLayer::Session,
                // The host profile accepts worker settings only (`validate_profile`).
                Layer::Managed => InputLayer::Global,
                Layer::Worker => continue,
            };
            if entry.value.is_null() {
                continue;
            }
            // A non-list (hand-edited file) is reported and skipped so the layer below
            // applies; treating it as `[]` would unbind the action (lotus#114).
            match layout::parse_list(action, &entry.value) {
                Ok((list, mut more)) => {
                    problems.append(&mut more);
                    overrides.insert(action, (list, layer));
                }
                Err(problem) => problems.push(problem),
            }
        }
        let stick = |name: &str| StickProfile {
            deadzone: float(&format!("lotus.input.{name}.deadzone"), 0.0),
            sensitivity: float(&format!("lotus.input.{name}.sensitivity"), 1.0),
            invert_y: get(&format!("lotus.input.{name}.invert_y"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        InputSettings {
            preset: get("lotus.input.preset")
                .and_then(Value::as_str)
                .map(str::to_string),
            overrides,
            profile: lotus_input::layout::Profile {
                sticks: [stick("circle_pad"), stick("c_stick")],
                threshold: float("lotus.input.threshold", 0.5),
            },
            problems,
        }
    }

    /// Layer a secondary's runtime overrides over its snapshot. The supervisor refuses runtime
    /// changes to managed keys, so the managed layer stays on top.
    pub fn apply_runtime(&mut self, values: &BTreeMap<String, Value>) {
        self.apply(Layer::Runtime, values);
    }

    /// Layer `values` on top, recording shadowed sources.
    fn apply(&mut self, layer: Layer, values: &BTreeMap<String, Value>) {
        for (key, value) in values {
            let Some(entry) = self.settings.get_mut(key) else {
                continue;
            };
            if entry.source != Layer::Default {
                entry.shadowed.push(entry.source);
            }
            entry.value = value.clone();
            entry.source = layer;
        }
    }
}

/// A session's input-layout settings, ready for `lotus_input::layout::resolve`.
#[derive(Debug, Clone, PartialEq)]
pub struct InputSettings {
    pub preset: Option<String>,
    pub overrides: lotus_input::layout::Overrides,
    pub profile: lotus_input::layout::Profile,
    pub problems: Vec<lotus_input::layout::Problem>,
}

fn owner(key: &str) -> Option<Owner> {
    Registry::builtin().get(key).map(|d| d.owner)
}

struct Loaded {
    values: Map<String, Value>,
    /// Why the file must not be written.
    unwritable: Option<String>,
}

fn load(path: &Path) -> Loaded {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Loaded {
                values: Map::new(),
                unwritable: None,
            }
        }
        Err(error) => {
            return Loaded {
                values: Map::new(),
                unwritable: Some(format!("{}: {error}", path.display())),
            }
        }
    };
    let refuse = |reason: String| Loaded {
        values: Map::new(),
        unwritable: Some(format!("{}: {reason}", path.display())),
    };
    let Ok(Value::Object(mut file)) = serde_json::from_slice::<Value>(&bytes) else {
        return refuse("not a settings object; left untouched".into());
    };
    match file.get("version").and_then(Value::as_u64) {
        Some(FILE_VERSION) => {}
        other => {
            return refuse(format!(
                "version {other:?}, this build reads {FILE_VERSION}; left untouched"
            ))
        }
    }
    match file.remove("values") {
        Some(Value::Object(values)) => Loaded {
            values,
            unwritable: None,
        },
        None => Loaded {
            values: Map::new(),
            unwritable: None,
        },
        Some(_) => refuse("values is not an object; left untouched".into()),
    }
}

/// Write `values` through a temporary file and rename it over `path`.
fn save(path: &Path, values: Map<String, Value>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = Map::new();
    file.insert("version".into(), FILE_VERSION.into());
    file.insert("values".into(), Value::Object(values));
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(&Value::Object(file)).map_err(io::Error::other)?,
    )?;
    fs::rename(&temporary, path)
}

/// Check a value for `key` at `scope`. A worker branch is not known for saved files, so a key
/// passes when either branch accepts it; the worker refuses a key it lacks at apply.
pub fn check(key: &str, value: &Value, scope: Scope, branch: Option<Branch>) -> io::Result<()> {
    let registry = Registry::builtin();
    let checked = match branch {
        Some(branch) => registry.validate(key, value, scope, Some(branch)),
        None => registry
            .validate(key, value, scope, Some(Branch::Integration))
            .or_else(|_| registry.validate(key, value, scope, Some(Branch::Experimental))),
    }
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    if !matches!(checked.owner, Owner::Azahar | Owner::Input) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{key} is a Lotus setting: host settings live in the host configuration, \
                 desktop settings in desktop.json"
            ),
        ));
    }
    Ok(())
}

/// Saved global defaults and card overrides of one storage root.
#[derive(Debug)]
pub struct SettingsStore {
    storage_root: PathBuf,
    managed: BTreeMap<String, Value>,
    writing: Mutex<()>,
}

impl SettingsStore {
    /// `managed` is the host configuration `profile`, already validated by the supervisor.
    pub fn new(storage_root: &Path, managed: &BTreeMap<String, SettingValue>) -> io::Result<Self> {
        Ok(Self {
            storage_root: storage_root.to_path_buf(),
            managed: managed
                .iter()
                .map(|(key, value)| {
                    serde_json::to_value(value)
                        .map(|value| (key.clone(), value))
                        .map_err(io::Error::other)
                })
                .collect::<io::Result<_>>()?,
            writing: Mutex::new(()),
        })
    }

    /// Host configuration `profile` entries.
    #[must_use]
    pub fn managed(&self) -> &BTreeMap<String, Value> {
        &self.managed
    }

    #[must_use]
    pub fn path(&self, store: &Store) -> PathBuf {
        match store {
            Store::Global => self.storage_root.join("settings").join("global.json"),
            Store::Card(card) => self.storage_root.join("cards").join(card).join(CARD_FILE),
        }
    }

    /// Read the usable entries of one store, reporting the rest into `resolution`.
    fn layer(&self, store: &Store, resolution: &mut Resolution) -> BTreeMap<String, Value> {
        let path = self.path(store);
        let loaded = load(&path);
        if let Some(reason) = loaded.unwritable {
            resolution.problems.push(Problem {
                layer: store.layer(),
                key: None,
                reason,
            });
            resolution.read_only.push(path);
        }
        let mut usable = BTreeMap::new();
        for (key, value) in loaded.values {
            if let Err(error) = check(&key, &value, store.scope(), None) {
                resolution.problems.push(Problem {
                    layer: store.layer(),
                    key: Some(key),
                    reason: format!("ignored: {error}"),
                });
                continue;
            }
            if self.managed.contains_key(&key) {
                resolution.problems.push(Problem {
                    layer: store.layer(),
                    key: Some(key.clone()),
                    reason: "shadowed by the host configuration profile".into(),
                });
            }
            usable.insert(key, value);
        }
        usable
    }

    /// Defaults, global and (for a primary card or a secondary's parent) card layers, then
    /// the managed profile.
    pub fn resolve(&self, card: Option<&str>) -> io::Result<Resolution> {
        let mut resolution = Resolution::default();
        for descriptor in Registry::builtin().descriptors() {
            let usable = matches!(descriptor.owner, Owner::Azahar | Owner::Input)
                && !matches!(descriptor.availability, Availability::Excluded { .. });
            if usable {
                resolution.settings.insert(
                    descriptor.key.clone(),
                    Entry {
                        value: descriptor.default.clone(),
                        source: Layer::Default,
                        shadowed: Vec::new(),
                    },
                );
            }
        }
        let global = self.layer(&Store::Global, &mut resolution);
        resolution.apply(Layer::Global, &global);
        if let Some(card) = card {
            crate::card_store::validate_name(card)?;
            let overrides = self.layer(&Store::Card(card.to_string()), &mut resolution);
            resolution.apply(Layer::Card, &overrides);
        }
        let managed = self.managed.clone();
        resolution.apply(Layer::Managed, &managed);
        Ok(resolution)
    }

    /// Set (`Some`) or remove (`None`) one saved entry. Other entries, including ones this
    /// build cannot use, are kept.
    pub fn set(&self, store: &Store, key: &str, value: Option<Value>) -> io::Result<()> {
        if let Store::Card(card) = store {
            crate::card_store::validate_name(card)?;
            if !self.storage_root.join("cards").join(card).is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("unknown card '{card}'"),
                ));
            }
        }
        if let Some(value) = &value {
            check(key, value, store.scope(), None)?;
            if self.managed.contains_key(key) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{key} is managed by the host configuration profile"),
                ));
            }
        }
        self.write(store, |values| {
            match value {
                Some(value) => values.insert(key.to_string(), value),
                None => values.remove(key),
            };
        })
    }

    /// Remove every saved entry of one store, including ones this build cannot use.
    pub fn reset(&self, store: &Store) -> io::Result<()> {
        self.write(store, Map::clear)
    }

    fn write(&self, store: &Store, change: impl FnOnce(&mut Map<String, Value>)) -> io::Result<()> {
        self.locked(|| {
            let path = self.path(store);
            let loaded = load(&path);
            if let Some(reason) = loaded.unwritable {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("refusing to overwrite {reason}"),
                ));
            }
            let mut values = loaded.values;
            change(&mut values);
            save(&path, values)
        })
    }

    /// Run `f` holding the in-process mutex and `settings/settings.lock`.
    fn locked<T>(&self, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let _guard = self
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lock_dir = self.storage_root.join("settings");
        fs::create_dir_all(&lock_dir)?;
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_dir.join("settings.lock"))?;
        lock.lock()?;
        f()
    }

    /// Saved input presets (lotus#63), one exported preset per file.
    #[must_use]
    pub fn presets_dir(&self) -> PathBuf {
        self.storage_root.join("settings").join("input-presets")
    }

    /// Saved presets by name, and files that could not be used (`file: reason`). A file's
    /// name is the preset's name; a preset whose `name` differs is renamed to the file's.
    #[must_use]
    pub fn input_presets(&self) -> (Vec<Preset>, Vec<String>) {
        let mut presets = Vec::new();
        let mut problems = Vec::new();
        let Ok(entries) = fs::read_dir(self.presets_dir()) else {
            return (presets, problems);
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let Some(stem) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
            else {
                continue;
            };
            let file = path.display();
            if preset_name(stem).is_err() || layout::is_builtin(stem) {
                problems.push(format!("{file}: not a usable preset name"));
                continue;
            }
            let loaded = fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|json| Preset::import(&json).map_err(|e| e.to_string()));
            match loaded {
                Ok((mut preset, dropped)) => {
                    if !dropped.is_empty() {
                        problems.push(format!("{file}: {dropped:?}"));
                    }
                    preset.name = stem.to_string();
                    presets.push(preset);
                }
                Err(reason) => problems.push(format!("{file}: {reason}")),
            }
        }
        (presets, problems)
    }

    /// Export a builtin or saved preset.
    pub fn export_preset(&self, name: &str) -> io::Result<String> {
        self.find_preset(name).map(|p| p.export())
    }

    fn find_preset(&self, name: &str) -> io::Result<Preset> {
        layout::builtins()
            .into_iter()
            .chain(self.input_presets().0)
            .find(|p| p.name == name)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("no input preset '{name}'"))
            })
    }

    /// Apply one library change; return the name of the preset it created or changed and,
    /// for an import or save, the problems [`Preset::import`] reported (lotus#114).
    /// Builtin presets cannot be replaced, renamed or deleted. Renaming or deleting a preset
    /// leaves selections of the old name in place; they fall back to `default` and report it.
    pub fn change_preset(
        &self,
        change: PresetChange,
    ) -> io::Result<(String, Vec<layout::Problem>)> {
        self.locked(|| {
            let dir = self.presets_dir();
            let path = |name: &str| dir.join(format!("{name}.json"));
            let writable = |name: &str| -> io::Result<()> {
                preset_name(name)?;
                if layout::is_builtin(name) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("'{name}' is a builtin preset"),
                    ));
                }
                Ok(())
            };
            let write = |preset: &Preset, replace: bool| -> io::Result<String> {
                writable(&preset.name)?;
                let target = path(&preset.name);
                if !replace && target.exists() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("input preset '{}' exists", preset.name),
                    ));
                }
                fs::create_dir_all(&dir)?;
                let temporary = target.with_extension("json.tmp");
                fs::write(&temporary, preset.export())?;
                fs::rename(&temporary, &target)?;
                Ok(preset.name.clone())
            };
            let import = |json: &str| {
                Preset::import(json)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
            };
            match change {
                PresetChange::Save { preset, replace } => {
                    let (preset, problems) = import(&preset.export())?;
                    // Never replace a file this build cannot read (a newer version, or one
                    // edited by hand): the user would lose it without having seen it.
                    let target = path(&preset.name);
                    if replace && target.exists() {
                        fs::read_to_string(&target)
                            .map_err(io::Error::other)
                            .and_then(|json| {
                                import(&json).map_err(|e| {
                                    io::Error::new(
                                        io::ErrorKind::InvalidData,
                                        format!("{} is not replaced: {e}", target.display()),
                                    )
                                })
                            })?;
                    }
                    Ok((write(&preset, replace)?, problems))
                }
                PresetChange::Import { json } => {
                    let (mut preset, problems) = import(&json)?;
                    preset_name(&preset.name)?;
                    preset.name = self.free_name(&preset.name)?;
                    Ok((write(&preset, false)?, problems))
                }
                PresetChange::Duplicate { from, to } => {
                    let mut preset = self.find_preset(&from)?;
                    preset.name = to;
                    Ok((write(&preset, false)?, Vec::new()))
                }
                PresetChange::Rename { from, to } => {
                    writable(&from)?;
                    let mut preset = self.find_preset(&from)?;
                    preset.name = to;
                    let name = write(&preset, false)?;
                    fs::remove_file(path(&from))?;
                    Ok((name, Vec::new()))
                }
                PresetChange::Delete { name } => {
                    writable(&name)?;
                    self.find_preset(&name)?;
                    fs::remove_file(path(&name))?;
                    Ok((name, Vec::new()))
                }
            }
        })
    }

    /// `name`, or `name-2`, `name-3`, ... (shortened to fit) when taken.
    fn free_name(&self, name: &str) -> io::Result<String> {
        let taken: Vec<String> = layout::builtins()
            .into_iter()
            .chain(self.input_presets().0)
            .map(|p| p.name)
            .collect();
        if !taken.iter().any(|t| t == name) {
            return Ok(name.to_string());
        }
        for n in 2..1000 {
            let suffix = format!("-{n}");
            let base: String = name.chars().take(32 - suffix.len()).collect();
            let candidate = format!("{base}{suffix}");
            if !taken.contains(&candidate) {
                return Ok(candidate);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("no free name for input preset '{name}'"),
        ))
    }
}

/// Preset names are file names: card-name rules (`card_store::validate_name`).
fn preset_name(name: &str) -> io::Result<()> {
    crate::card_store::validate_name(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("input preset names are 1-32 of a-z, 0-9, '-', '_' and start with a letter or digit, not '{name}'"),
        )
    })
}

/// One change to the saved preset library.
#[derive(Debug, Clone)]
pub enum PresetChange {
    /// Create, or with `replace` overwrite, a saved preset.
    Save {
        preset: Preset,
        replace: bool,
    },
    /// An exported preset; a taken name gets a `-N` suffix.
    Import {
        json: String,
    },
    Duplicate {
        from: String,
        to: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Delete {
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lotus-settings-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cards/a")).unwrap();
        fs::create_dir_all(root.join("cards/b")).unwrap();
        root
    }

    fn value(resolution: &Resolution, key: &str) -> (Value, Layer) {
        let entry = &resolution.settings[key];
        (entry.value.clone(), entry.source)
    }

    #[test]
    fn cards_inherit_global_until_they_override_and_removal_restores_it() {
        let root = root("inherit");
        let store = SettingsStore::new(&root, &BTreeMap::new()).unwrap();
        let card_a = Store::Card("a".into());
        store
            .set(&Store::Global, "cpu_clock_percentage", Some(json!(150)))
            .unwrap();
        store
            .set(&card_a, "cpu_clock_percentage", Some(json!(50)))
            .unwrap();
        let a = store.resolve(Some("a")).unwrap();
        let b = store.resolve(Some("b")).unwrap();
        assert_eq!(value(&a, "cpu_clock_percentage"), (json!(50), Layer::Card));
        assert_eq!(a.settings["cpu_clock_percentage"].shadowed, [Layer::Global]);
        assert_eq!(
            value(&b, "cpu_clock_percentage"),
            (json!(150), Layer::Global)
        );
        // A changed global reaches the inheriting card only.
        store
            .set(&Store::Global, "cpu_clock_percentage", Some(json!(200)))
            .unwrap();
        assert_eq!(
            value(&store.resolve(Some("b")).unwrap(), "cpu_clock_percentage").0,
            json!(200)
        );
        assert_eq!(
            value(&store.resolve(Some("a")).unwrap(), "cpu_clock_percentage").0,
            json!(50)
        );
        // Removal, seen by a fresh store as after a restart.
        store.set(&card_a, "cpu_clock_percentage", None).unwrap();
        let restarted = SettingsStore::new(&root, &BTreeMap::new()).unwrap();
        assert_eq!(
            value(
                &restarted.resolve(Some("a")).unwrap(),
                "cpu_clock_percentage"
            ),
            (json!(200), Layer::Global)
        );
        // Only explicit entries are saved.
        let saved: Value =
            serde_json::from_slice(&fs::read(root.join("cards/a").join(CARD_FILE)).unwrap())
                .unwrap();
        assert_eq!(saved, json!({"version": 1, "values": {}}));
        let profile = restarted.resolve(Some("a")).unwrap().profile();
        assert_eq!(profile.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn managed_profile_wins_and_cannot_be_edited() {
        let root = root("managed");
        let unmanaged = SettingsStore::new(&root, &BTreeMap::new()).unwrap();
        unmanaged
            .set(&Store::Global, "graphics_api", Some(json!(2)))
            .unwrap();
        let managed = SettingsStore::new(
            &root,
            &BTreeMap::from([("graphics_api".to_string(), SettingValue::Integer(0))]),
        )
        .unwrap();
        let resolution = managed.resolve(None).unwrap();
        assert_eq!(
            value(&resolution, "graphics_api"),
            (json!(0), Layer::Managed)
        );
        assert!(resolution
            .problems
            .iter()
            .any(|p| p.key.as_deref() == Some("graphics_api") && p.reason.contains("shadowed")));
        let error = managed
            .set(&Store::Card("a".into()), "graphics_api", Some(json!(2)))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        // The shadowed saved entry is kept.
        assert_eq!(
            value(&unmanaged.resolve(None).unwrap(), "graphics_api").0,
            json!(2)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn refuses_bad_entries_and_keeps_what_it_cannot_use() {
        let root = root("refuse");
        let store = SettingsStore::new(&root, &BTreeMap::new()).unwrap();
        for (key, value) in [
            ("no_such_setting", json!(1)),
            ("use_gdbstub", json!(false)),
            ("cpu_clock_percentage", json!(401)),
            ("lotus.host.prefer_vulkan", json!(true)),
        ] {
            let error = store.set(&Store::Global, key, Some(value)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{key}");
        }
        assert_eq!(
            store
                .set(&Store::Card("missing".into()), "volume", Some(json!(0.5)))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        // An entry from a newer build: reported, ignored, and kept across writes.
        let path = store.path(&Store::Global);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            br#"{"version":1,"values":{"future_setting":true,"volume":0.25}}"#,
        )
        .unwrap();
        let resolution = store.resolve(None).unwrap();
        assert_eq!(value(&resolution, "volume"), (json!(0.25), Layer::Global));
        assert!(resolution
            .problems
            .iter()
            .any(|p| p.key.as_deref() == Some("future_setting")));
        store
            .set(&Store::Global, "volume", Some(json!(0.5)))
            .unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["values"]["future_setting"], json!(true));
        store.reset(&Store::Global).unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["values"], json!({}));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_and_newer_files_are_reported_and_never_written() {
        let root = root("malformed");
        let store = SettingsStore::new(&root, &BTreeMap::new()).unwrap();
        let path = store.path(&Store::Card("a".into()));
        for contents in [
            &b"{\"version\":1,\"values\":{\"vol"[..],
            br#"{"version":2,"values":{}}"#,
        ] {
            fs::write(&path, contents).unwrap();
            let resolution = store.resolve(Some("a")).unwrap();
            assert_eq!(resolution.read_only, std::slice::from_ref(&path));
            assert_eq!(resolution.problems.len(), 1, "{:?}", resolution.problems);
            let error = store
                .set(&Store::Card("a".into()), "volume", Some(json!(0.5)))
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(store.reset(&Store::Card("a".into())).is_err());
            assert_eq!(fs::read(&path).unwrap(), contents);
        }
        // The global layer still resolves.
        store
            .set(&Store::Global, "volume", Some(json!(0.5)))
            .unwrap();
        assert_eq!(
            value(&store.resolve(Some("a")).unwrap(), "volume"),
            (json!(0.5), Layer::Global)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_crash_mid_write_leaves_the_previous_file() {
        let root = root("crash");
        let store = SettingsStore::new(&root, &BTreeMap::new()).unwrap();
        store
            .set(&Store::Global, "volume", Some(json!(0.5)))
            .unwrap();
        let path = store.path(&Store::Global);
        // A crash before the rename leaves a partial temporary file and the old file.
        fs::write(path.with_extension("json.tmp"), b"{\"version\":1,\"val").unwrap();
        let restarted = SettingsStore::new(&root, &BTreeMap::new()).unwrap();
        let resolution = restarted.resolve(None).unwrap();
        assert_eq!(value(&resolution, "volume"), (json!(0.5), Layer::Global));
        assert!(resolution.problems.is_empty());
        restarted
            .set(&Store::Global, "frame_limit", Some(json!(200)))
            .unwrap();
        let resolution = restarted.resolve(None).unwrap();
        assert_eq!(value(&resolution, "volume").0, json!(0.5));
        assert_eq!(value(&resolution, "frame_limit").0, json!(200));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_writers_keep_each_others_entries() {
        let root = root("concurrent");
        // Two stores on one root stand in for two Lotus processes; each takes the file lock.
        let stores = [
            std::sync::Arc::new(SettingsStore::new(&root, &BTreeMap::new()).unwrap()),
            std::sync::Arc::new(SettingsStore::new(&root, &BTreeMap::new()).unwrap()),
        ];
        let keys: Vec<String> = Registry::builtin()
            .descriptors()
            .iter()
            .filter(|d| {
                d.owner == Owner::Azahar
                    && d.kind == lotus_settings::Kind::Boolean
                    && check(&d.key, &json!(true), Scope::Global, None).is_ok()
            })
            .take(16)
            .map(|d| d.key.clone())
            .collect();
        assert_eq!(keys.len(), 16);
        let threads: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let store = stores[i % 2].clone();
                let key = key.clone();
                std::thread::spawn(move || store.set(&Store::Global, &key, Some(json!(true))))
            })
            .collect();
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        let saved: Value =
            serde_json::from_slice(&fs::read(stores[0].path(&Store::Global)).unwrap()).unwrap();
        assert_eq!(saved["values"].as_object().unwrap().len(), keys.len());
        let _ = fs::remove_dir_all(&root);
    }

    fn key(k: &str) -> Value {
        json!([{"device": "keyboard", "key": k}])
    }

    #[test]
    fn input_keys_layer_per_action_and_never_reach_a_worker_profile() {
        use lotus_input::layout::{resolve, Action, Layer as InputLayer};
        let root = root("input-layers");
        let store = SettingsStore::new(&root, &BTreeMap::new()).unwrap();
        let card_a = Store::Card("a".into());
        store
            .set(&Store::Global, "lotus.input.bind.a", Some(key("j")))
            .unwrap();
        store
            .set(
                &Store::Global,
                "lotus.input.preset",
                Some(json!("xbox-labels")),
            )
            .unwrap();
        store
            .set(&card_a, "lotus.input.bind.b", Some(key("k")))
            .unwrap();
        store
            .set(
                &card_a,
                "lotus.input.circle_pad.deadzone",
                Some(json!(0.25)),
            )
            .unwrap();
        store
            .set(&Store::Global, "cpu_clock_percentage", Some(json!(150)))
            .unwrap();

        let a = store.resolve(Some("a")).unwrap();
        let profile = a.profile();
        assert!(profile.contains_key("cpu_clock_percentage"));
        assert!(
            profile.keys().all(|k| !k.starts_with("lotus.")),
            "{profile:?}"
        );

        let input = a.input();
        assert_eq!(input.preset.as_deref(), Some("xbox-labels"));
        assert_eq!(input.overrides[&Action::A].1, InputLayer::Global);
        assert_eq!(input.overrides[&Action::B].1, InputLayer::Card);
        assert_eq!(input.profile.sticks[0].deadzone, 0.25);
        assert_eq!(input.profile.threshold, 0.5);
        let resolved = resolve(&[], input.preset.as_deref(), &input.overrides);
        assert_eq!(resolved.key_bits("j"), lotus_input::router::bits::A);
        assert_eq!(resolved.key_bits("k"), lotus_input::router::bits::B);

        // Card b inherits only the global layer; a secondary's runtime override sits on top
        // and its removal restores the card's.
        let b = store.resolve(Some("b")).unwrap().input();
        assert!(!b.overrides.contains_key(&Action::B));
        let mut runtime = store.resolve(Some("a")).unwrap();
        runtime.apply_runtime(&BTreeMap::from([("lotus.input.bind.b".into(), key("l"))]));
        assert_eq!(
            runtime.input().overrides[&Action::B],
            (
                vec![lotus_input::layout::Binding::Keyboard { key: "l".into() }],
                InputLayer::Session
            )
        );
        assert_eq!(
            store.resolve(Some("a")).unwrap().input().overrides[&Action::B].1,
            InputLayer::Card
        );

        // A bad binding is refused at every target.
        let bad = json!([{"device": "wheel"}]);
        assert!(store.set(&card_a, "lotus.input.bind.x", Some(bad)).is_err());
    }

    #[test]
    fn input_preset_library_round_trip() {
        use lotus_input::layout::Preset;
        let root = root("input-presets");
        let store = SettingsStore::new(&root, &BTreeMap::new()).unwrap();
        let mut mine = Preset::builtin();
        mine.name = "mine".into();
        mine.bindings.insert(lotus_input::layout::Action::A, vec![]);
        let save = |preset: &Preset, replace| {
            store.change_preset(PresetChange::Save {
                preset: preset.clone(),
                replace,
            })
        };
        assert_eq!(save(&mine, false).unwrap(), ("mine".to_string(), vec![]));
        assert_eq!(
            save(&mine, false).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        save(&mine, true).unwrap();
        let mut builtin = Preset::builtin();
        assert_eq!(
            save(&builtin, true).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        builtin.name = "My Pad".into();
        assert_eq!(
            save(&builtin, false).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        // Import of a taken name gets a suffix; export/import keeps the bindings.
        let exported = store.export_preset("mine").unwrap();
        let imported = store
            .change_preset(PresetChange::Import {
                json: exported.clone(),
            })
            .unwrap()
            .0;
        assert_eq!(imported, "mine-2");
        let json = store.export_preset("xbox-labels").unwrap();
        assert_eq!(
            store
                .change_preset(PresetChange::Import { json })
                .unwrap()
                .0,
            "xbox-labels-2"
        );
        assert!(store
            .change_preset(PresetChange::Import {
                json: r#"{"version":9,"name":"x"}"#.into()
            })
            .is_err());

        store
            .change_preset(PresetChange::Duplicate {
                from: "default".into(),
                to: "copy".into(),
            })
            .unwrap();
        store
            .change_preset(PresetChange::Rename {
                from: "copy".into(),
                to: "renamed".into(),
            })
            .unwrap();
        assert!(store
            .change_preset(PresetChange::Rename {
                from: "default".into(),
                to: "x".into()
            })
            .is_err());
        store
            .change_preset(PresetChange::Delete {
                name: "mine-2".into(),
            })
            .unwrap();
        assert!(store
            .change_preset(PresetChange::Delete {
                name: "default".into()
            })
            .is_err());

        // lotus#114: a list of the wrong type is refused and nothing is written; dropped
        // entries come back as warnings.
        let malformed = r#"{"version":1,"name":"broken-a","bindings":{"a":null}}"#;
        let error = store
            .change_preset(PresetChange::Import {
                json: malformed.into(),
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("bindings.a"), "{error}");
        assert!(!store.presets_dir().join("broken-a.json").exists());
        let (name, warnings) = store
            .change_preset(PresetChange::Import {
                json: r#"{"version":1,"name":"partial","bindings":{
                    "a":[{"device":"wheel"}],"teleport":[]}}"#
                    .into(),
            })
            .unwrap();
        assert_eq!(name, "partial");
        use lotus_input::layout::Problem as P;
        assert!(warnings.contains(&P::UnknownAction("teleport".into())));
        assert!(warnings
            .iter()
            .any(|p| matches!(p, P::InvalidBinding { .. })));
        assert!(warnings.iter().any(|p| matches!(p, P::MissingAction(_))));
        fs::remove_file(store.presets_dir().join("partial.json")).unwrap();

        // Save never replaces a file this build cannot read.
        let newer = store.presets_dir().join("newer.json");
        fs::write(&newer, r#"{"version":9,"name":"newer"}"#).unwrap();
        let mut over = Preset::builtin();
        over.name = "newer".into();
        assert_eq!(
            save(&over, true).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            fs::read_to_string(&newer).unwrap(),
            r#"{"version":9,"name":"newer"}"#
        );
        fs::remove_file(&newer).unwrap();

        // A stray file with a bad name or content is reported, not loaded.
        fs::write(store.presets_dir().join("Bad Name.json"), "{}").unwrap();
        fs::write(store.presets_dir().join("broken.json"), "{").unwrap();
        let (presets, problems) = store.input_presets();
        let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["mine", "renamed", "xbox-labels-2"]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert_eq!(presets[0].bindings[&lotus_input::layout::Action::A], vec![]);
        let _ = fs::remove_dir_all(&root);
    }
}
