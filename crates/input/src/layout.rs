//! Named input-layout presets (lotus#57): which physical inputs drive which 3DS action.
//!
//! A preset binds stable action IDs to portable binding descriptors: GPUI key names for the
//! keyboard (the strings `lotus_window.rs` matches today, not platform scancodes) and the
//! positional [`Button`]/[`Axis`] names for controllers (not backend IDs). Layers resolve like
//! worker settings (lotus#51): builtin → global → card → a secondary's ephemeral overrides.
//! See `docs/30-sessions-and-interface.md` "Input layouts (#57)".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::router::bits;
use crate::{Axis, Button};

/// Preset file format version.
pub const VERSION: u32 = 1;
/// Name of the builtin preset, which reproduces the shipped defaults.
pub const BUILTIN: &str = "default";

/// Emulated 3DS inputs a preset can bind. The serialized IDs are stable; renaming one is a
/// format change. Touch is not here: it follows the pointer on the bottom screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    A,
    B,
    X,
    Y,
    Up,
    Down,
    Left,
    Right,
    L,
    R,
    Zl,
    Zr,
    Start,
    Select,
    Home,
    CirclePadUp,
    CirclePadDown,
    CirclePadLeft,
    CirclePadRight,
    CStickUp,
    CStickDown,
    CStickLeft,
    CStickRight,
}

impl Action {
    pub const ALL: [Action; 23] = [
        Action::A,
        Action::B,
        Action::X,
        Action::Y,
        Action::Up,
        Action::Down,
        Action::Left,
        Action::Right,
        Action::L,
        Action::R,
        Action::Zl,
        Action::Zr,
        Action::Start,
        Action::Select,
        Action::Home,
        Action::CirclePadUp,
        Action::CirclePadDown,
        Action::CirclePadLeft,
        Action::CirclePadRight,
        Action::CStickUp,
        Action::CStickDown,
        Action::CStickLeft,
        Action::CStickRight,
    ];

    /// Button bit (`NativeButton` order, [`bits`]), or `None` for a stick direction.
    #[must_use]
    pub fn bit(self) -> Option<u32> {
        Some(match self {
            Action::A => bits::A,
            Action::B => bits::B,
            Action::X => bits::X,
            Action::Y => bits::Y,
            Action::Up => bits::UP,
            Action::Down => bits::DOWN,
            Action::Left => bits::LEFT,
            Action::Right => bits::RIGHT,
            Action::L => bits::L,
            Action::R => bits::R,
            Action::Zl => bits::ZL,
            Action::Zr => bits::ZR,
            Action::Start => bits::START,
            Action::Select => bits::SELECT,
            Action::Home => bits::HOME,
            _ => return None,
        })
    }

    /// Stable serialized ID (`circle_pad_up`).
    #[must_use]
    pub fn id(self) -> String {
        match serde_json::to_value(self) {
            Ok(Value::String(id)) => id,
            _ => unreachable!("unit variants serialize as strings"),
        }
    }
}

/// Which way an axis must move to press a direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Positive,
    Negative,
}

/// One physical input. `device` is the capability a binding needs; a binding for a device
/// that is not present is kept and simply never fires.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "device", rename_all = "snake_case")]
pub enum Binding {
    /// GPUI key name, as `Keystroke::key` reports it (`z`, `enter`, `up`).
    Keyboard { key: String },
    /// Controller button by position.
    Button { button: Button },
    /// Controller axis past its threshold in one direction.
    Axis { axis: Axis, direction: Direction },
}

fn key(key: &str) -> Binding {
    Binding::Keyboard { key: key.into() }
}

fn pad(button: Button) -> Binding {
    Binding::Button { button }
}

fn axis(axis: Axis, direction: Direction) -> Binding {
    Binding::Axis { axis, direction }
}

/// A named, complete layout: every action is present, possibly with no bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preset {
    pub version: u32,
    pub name: String,
    pub bindings: BTreeMap<Action, Vec<Binding>>,
}

/// Something a load or resolve could not use; reported, never fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// An action ID this build does not know; its bindings were ignored.
    UnknownAction(String),
    /// A binding this build cannot parse (unknown device, button or axis); ignored.
    InvalidBinding { action: Action, binding: Value },
    /// An action the file does not list; it took the builtin bindings.
    MissingAction(Action),
    /// A selected preset that is not in the library; the builtin was used instead.
    MissingPreset(String),
}

/// Why a preset file cannot be loaded at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    NotJson(String),
    /// Missing, zero or newer than [`VERSION`]; the file must not be rewritten.
    Version(Option<u64>),
    NoName,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotJson(error) => write!(f, "not a preset: {error}"),
            LoadError::Version(Some(v)) => {
                write!(f, "preset version {v}, this build reads {VERSION}")
            }
            LoadError::Version(None) => write!(f, "preset has no version"),
            LoadError::NoName => write!(f, "preset has no name"),
        }
    }
}

impl std::error::Error for LoadError {}

impl Preset {
    /// The shipped controls. Keyboard: `lotus_window.rs` `button` before lotus#57 (Z/X A/B,
    /// A/S X/Y, Q/W L/R, arrows, Enter/Backspace Start/Select). Controller:
    /// [`crate::router::default_button`] and the left/right sticks on the Circle Pad/C-stick.
    #[must_use]
    pub fn builtin() -> Self {
        use Direction::{Negative, Positive};
        let keyboard = [
            (Action::A, "z"),
            (Action::B, "x"),
            (Action::X, "a"),
            (Action::Y, "s"),
            (Action::Up, "up"),
            (Action::Down, "down"),
            (Action::Left, "left"),
            (Action::Right, "right"),
            (Action::L, "q"),
            (Action::R, "w"),
            (Action::Start, "enter"),
            (Action::Select, "backspace"),
        ];
        let mut bindings: BTreeMap<Action, Vec<Binding>> =
            Action::ALL.iter().map(|a| (*a, Vec::new())).collect();
        for (action, name) in keyboard {
            bindings.entry(action).or_default().push(key(name));
        }
        const BUTTONS: [Button; 17] = [
            Button::South,
            Button::East,
            Button::North,
            Button::West,
            Button::LeftShoulder,
            Button::RightShoulder,
            Button::LeftTrigger,
            Button::RightTrigger,
            Button::Select,
            Button::Start,
            Button::Mode,
            Button::LeftThumb,
            Button::RightThumb,
            Button::DpadUp,
            Button::DpadDown,
            Button::DpadLeft,
            Button::DpadRight,
        ];
        for button in BUTTONS {
            let Some(bit) = crate::router::default_button(button) else {
                continue;
            };
            if let Some(action) = Action::ALL.iter().find(|a| a.bit() == Some(bit)) {
                bindings.entry(*action).or_default().push(pad(button));
            }
        }
        for (action, a, d) in [
            (Action::CirclePadUp, Axis::LeftY, Positive),
            (Action::CirclePadDown, Axis::LeftY, Negative),
            (Action::CirclePadLeft, Axis::LeftX, Negative),
            (Action::CirclePadRight, Axis::LeftX, Positive),
            (Action::CStickUp, Axis::RightY, Positive),
            (Action::CStickDown, Axis::RightY, Negative),
            (Action::CStickLeft, Axis::RightX, Negative),
            (Action::CStickRight, Axis::RightX, Positive),
        ] {
            bindings.entry(action).or_default().push(axis(a, d));
        }
        Self {
            version: VERSION,
            name: BUILTIN.into(),
            bindings,
        }
    }

    /// Parse an exported preset. Unknown actions and unparseable bindings are dropped and
    /// reported; actions the file omits (a preset from before an action existed) take the
    /// builtin bindings. A wrong version or a non-preset is refused.
    pub fn import(json: &str) -> Result<(Self, Vec<Problem>), LoadError> {
        let value: Value =
            serde_json::from_str(json).map_err(|e| LoadError::NotJson(e.to_string()))?;
        let Value::Object(file) = value else {
            return Err(LoadError::NotJson("not an object".into()));
        };
        let version = file.get("version").and_then(Value::as_u64);
        if !matches!(version, Some(v) if v >= 1 && v <= u64::from(VERSION)) {
            return Err(LoadError::Version(version));
        }
        let name = match file.get("name") {
            Some(Value::String(name)) if !name.trim().is_empty() => name.clone(),
            _ => return Err(LoadError::NoName),
        };
        let mut problems = Vec::new();
        let listed = match file.get("bindings") {
            Some(Value::Object(map)) => map.clone(),
            _ => Map::new(),
        };
        let (bindings, mut more) = parse_bindings(&listed);
        problems.append(&mut more);
        let builtin = Self::builtin();
        let mut complete = BTreeMap::new();
        for action in Action::ALL {
            match bindings.get(&action) {
                Some(list) => {
                    complete.insert(action, list.clone());
                }
                None => {
                    problems.push(Problem::MissingAction(action));
                    complete.insert(action, builtin.bindings[&action].clone());
                }
            }
        }
        Ok((
            Self {
                version: VERSION,
                name,
                bindings: complete,
            },
            problems,
        ))
    }

    /// Portable JSON, the input of [`Preset::import`].
    #[must_use]
    pub fn export(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

/// Parse `{"action_id": [binding, ...]}`, keeping what this build understands.
fn parse_bindings(map: &Map<String, Value>) -> (BTreeMap<Action, Vec<Binding>>, Vec<Problem>) {
    let mut problems = Vec::new();
    let mut out = BTreeMap::new();
    for (id, list) in map {
        let Ok(action) = serde_json::from_value::<Action>(Value::String(id.clone())) else {
            problems.push(Problem::UnknownAction(id.clone()));
            continue;
        };
        let mut parsed = Vec::new();
        for binding in list.as_array().into_iter().flatten() {
            match serde_json::from_value::<Binding>(binding.clone()) {
                Ok(b) if !parsed.contains(&b) => parsed.push(b),
                Ok(_) => {}
                Err(_) => problems.push(Problem::InvalidBinding {
                    action,
                    binding: binding.clone(),
                }),
            }
        }
        out.insert(action, parsed);
    }
    (out, problems)
}

/// Sparse per-action replacements. An entry replaces that action's whole binding list; an
/// empty list unbinds it. Removing the entry (reset) inherits again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Overrides(pub BTreeMap<Action, Vec<Binding>>);

impl Overrides {
    /// Parse a stored overrides object; see [`Preset::import`] for what is dropped.
    #[must_use]
    pub fn import(value: &Value) -> (Self, Vec<Problem>) {
        let empty = Map::new();
        let (map, problems) = parse_bindings(value.as_object().unwrap_or(&empty));
        (Self(map), problems)
    }
}

/// A saved layer: an optional preset selection and overrides on top of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Selection {
    pub preset: Option<String>,
    pub overrides: Overrides,
}

/// Where a resolved action's bindings came from, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// The selected preset (builtin, or one the global or card layer selected).
    Preset,
    Global,
    Card,
    Session,
}

/// The layout one session uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Name of the preset the layers start from.
    pub preset: String,
    pub bindings: BTreeMap<Action, (Vec<Binding>, Layer)>,
    pub problems: Vec<Problem>,
}

/// Resolve one session's layout. The base preset is the card's selection, else the global
/// one, else [`BUILTIN`]. Global overrides are edits of the global preset, so a card that
/// selects a different preset drops them; card overrides then apply, then a secondary's
/// ephemeral `session` overrides. Nothing here writes a layer, so a secondary's edits cannot
/// reach its parent card or the global layer.
#[must_use]
pub fn resolve(
    library: &[Preset],
    global: &Selection,
    card: Option<&Selection>,
    session: Option<&Overrides>,
) -> Resolved {
    let mut problems = Vec::new();
    let card_selects = card
        .and_then(|c| c.preset.as_deref())
        .filter(|name| Some(*name) != global.preset.as_deref());
    let wanted = card_selects.or(global.preset.as_deref());
    let base = match wanted {
        None | Some(BUILTIN) => Preset::builtin(),
        Some(name) => library
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .unwrap_or_else(|| {
                problems.push(Problem::MissingPreset(name.into()));
                Preset::builtin()
            }),
    };
    let mut bindings: BTreeMap<Action, (Vec<Binding>, Layer)> = base
        .bindings
        .iter()
        .map(|(a, list)| (*a, (list.clone(), Layer::Preset)))
        .collect();
    let mut layers = Vec::new();
    if card_selects.is_none() {
        layers.push((Layer::Global, &global.overrides));
    }
    if let Some(card) = card {
        layers.push((Layer::Card, &card.overrides));
    }
    if let Some(session) = session {
        layers.push((Layer::Session, session));
    }
    for (layer, overrides) in layers {
        for (action, list) in &overrides.0 {
            bindings.insert(*action, (list.clone(), layer));
        }
    }
    Resolved {
        preset: base.name,
        bindings,
        problems,
    }
}

impl Resolved {
    /// Button bits a keyboard key presses; 0 when it is unbound.
    #[must_use]
    pub fn key_bits(&self, key: &str) -> u32 {
        let wanted = Binding::Keyboard { key: key.into() };
        self.bindings
            .iter()
            .filter(|(_, (list, _))| list.contains(&wanted))
            .filter_map(|(action, _)| action.bit())
            .fold(0, |bits, bit| bits | bit)
    }

    /// Bindings shared by more than one action, for the editor's conflict warning. A shared
    /// binding presses every action it is bound to.
    #[must_use]
    pub fn conflicts(&self) -> Vec<(Binding, Vec<Action>)> {
        let mut by_binding: BTreeMap<&Binding, Vec<Action>> = BTreeMap::new();
        for (action, (list, _)) in &self.bindings {
            for binding in list {
                by_binding.entry(binding).or_default().push(*action);
            }
        }
        by_binding
            .into_iter()
            .filter(|(_, actions)| actions.len() > 1)
            .map(|(binding, actions)| (binding.clone(), actions))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Resolved {
        resolve(&[], &Selection::default(), None, None)
    }

    #[test]
    fn builtin_keyboard_matches_shipped_keys() {
        // `lotus_window.rs` `button` before lotus#57.
        let shipped = [
            ("z", 1),
            ("x", 2),
            ("a", 4),
            ("s", 8),
            ("up", 16),
            ("down", 32),
            ("left", 64),
            ("right", 128),
            ("q", 256),
            ("w", 512),
            ("enter", 1024),
            ("backspace", 2048),
        ];
        let resolved = builtin();
        for (key, bits) in shipped {
            assert_eq!(resolved.key_bits(key), bits, "{key}");
        }
        assert_eq!(resolved.key_bits("e"), 0);
        assert_eq!(resolved.key_bits("space"), 0, "space runs/pauses");
        assert!(resolved.conflicts().is_empty());
    }

    #[test]
    fn builtin_controller_matches_default_button() {
        let resolved = builtin();
        for button in [
            Button::East,
            Button::LeftTrigger,
            Button::Mode,
            Button::DpadUp,
        ] {
            let bit = crate::router::default_button(button).unwrap();
            let bound: Vec<Action> = resolved
                .bindings
                .iter()
                .filter(|(_, (l, _))| l.contains(&Binding::Button { button }))
                .map(|(a, _)| *a)
                .collect();
            assert_eq!(bound.len(), 1, "{button:?}");
            assert_eq!(bound[0].bit(), Some(bit));
        }
        assert!(!resolved
            .bindings
            .values()
            .any(|(l, _)| l.contains(&pad(Button::LeftThumb))));
    }

    #[test]
    fn export_import_round_trips() {
        let preset = Preset::builtin();
        let (back, problems) = Preset::import(&preset.export()).unwrap();
        assert_eq!(back, preset);
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn import_tolerates_unknown_and_missing_and_refuses_versions() {
        let json = r#"{"version":1,"name":"mine","bindings":{
            "a":[{"device":"keyboard","key":"k"},{"device":"keyboard","key":"k"}],
            "b":[{"device":"wheel","spin":1},{"device":"button","button":"south"}],
            "teleport":[{"device":"keyboard","key":"t"}]}}"#;
        let (preset, problems) = Preset::import(json).unwrap();
        assert_eq!(
            preset.bindings[&Action::A],
            vec![key("k")],
            "duplicate dropped"
        );
        assert_eq!(preset.bindings[&Action::B], vec![pad(Button::South)]);
        assert_eq!(
            preset.bindings[&Action::X],
            Preset::builtin().bindings[&Action::X]
        );
        assert!(problems.contains(&Problem::UnknownAction("teleport".into())));
        assert!(problems.iter().any(|p| matches!(
            p,
            Problem::InvalidBinding {
                action: Action::B,
                ..
            }
        )));
        assert!(problems.contains(&Problem::MissingAction(Action::X)));
        assert_eq!(preset.bindings.len(), Action::ALL.len());

        for (json, error) in [
            (r#"{"version":2,"name":"n"}"#, LoadError::Version(Some(2))),
            (r#"{"version":0,"name":"n"}"#, LoadError::Version(Some(0))),
            (r#"{"name":"n"}"#, LoadError::Version(None)),
            (r#"{"version":1,"name":" "}"#, LoadError::NoName),
        ] {
            assert_eq!(Preset::import(json).unwrap_err(), error);
        }
        assert!(matches!(Preset::import("[]"), Err(LoadError::NotJson(_))));
    }

    #[test]
    fn layers_override_and_reset_per_action() {
        let global = Selection {
            preset: None,
            overrides: Overrides(BTreeMap::from([(Action::A, vec![key("j")])])),
        };
        let card = Selection {
            preset: None,
            overrides: Overrides(BTreeMap::from([(Action::B, vec![key("k")])])),
        };
        let session = Overrides(BTreeMap::from([
            (Action::B, vec![key("l")]),
            (Action::Start, vec![]),
        ]));
        let resolved = resolve(&[], &global, Some(&card), Some(&session));
        assert_eq!(resolved.bindings[&Action::A].1, Layer::Global);
        assert_eq!(resolved.key_bits("j"), bits::A);
        assert_eq!(resolved.key_bits("z"), 0, "A replaced, not added to");
        assert_eq!(
            resolved.bindings[&Action::B],
            (vec![key("l")], Layer::Session)
        );
        assert_eq!(resolved.key_bits("k"), 0);
        assert_eq!(resolved.key_bits("enter"), 0, "unbound by an empty list");
        assert_eq!(resolved.bindings[&Action::Y].1, Layer::Preset);

        // Reset of the session layer: the card's binding returns.
        let parent = resolve(&[], &global, Some(&card), None);
        assert_eq!(parent.key_bits("k"), bits::B);
        assert_eq!(parent.key_bits("enter"), bits::START);
    }

    #[test]
    fn secondary_override_leaves_parent_untouched() {
        let global = Selection::default();
        let card = Selection {
            preset: None,
            overrides: Overrides(BTreeMap::from([(Action::L, vec![key("1")])])),
        };
        let before = resolve(&[], &global, Some(&card), None);
        let mut session = Overrides::default();
        session.0.insert(Action::L, vec![key("2")]);
        let secondary = resolve(&[], &global, Some(&card), Some(&session));
        assert_eq!(secondary.key_bits("2"), bits::L);
        assert_eq!(resolve(&[], &global, Some(&card), None), before);
        assert_eq!(card.overrides.0[&Action::L], vec![key("1")]);
    }

    #[test]
    fn card_preset_replaces_global_overrides_and_missing_falls_back() {
        let mut lefty = Preset::builtin();
        lefty.name = "lefty".into();
        lefty.bindings.insert(Action::A, vec![key("m")]);
        let global = Selection {
            preset: None,
            overrides: Overrides(BTreeMap::from([(Action::B, vec![key("k")])])),
        };
        let card = Selection {
            preset: Some("lefty".into()),
            overrides: Overrides::default(),
        };
        let resolved = resolve(std::slice::from_ref(&lefty), &global, Some(&card), None);
        assert_eq!(resolved.preset, "lefty");
        assert_eq!(resolved.key_bits("m"), bits::A);
        assert_eq!(resolved.key_bits("x"), bits::B, "global override dropped");

        let gone = Selection {
            preset: Some("deleted".into()),
            overrides: Overrides::default(),
        };
        let resolved = resolve(&[], &gone, None, None);
        assert_eq!(resolved.preset, BUILTIN);
        assert_eq!(
            resolved.problems,
            vec![Problem::MissingPreset("deleted".into())]
        );
        assert_eq!(resolved.key_bits("z"), bits::A);
    }

    #[test]
    fn conflicts_are_reported_and_press_both() {
        let card = Selection {
            preset: None,
            overrides: Overrides(BTreeMap::from([(Action::B, vec![key("z")])])),
        };
        let resolved = resolve(&[], &Selection::default(), Some(&card), None);
        assert_eq!(
            resolved.conflicts(),
            vec![(key("z"), vec![Action::A, Action::B])]
        );
        assert_eq!(resolved.key_bits("z"), bits::A | bits::B);
    }

    #[test]
    fn overrides_import_reports_unknown() {
        let value = serde_json::json!({"zl":[{"device":"axis","axis":"left_z","direction":"positive"}],"nope":[]});
        let (overrides, problems) = Overrides::import(&value);
        assert_eq!(
            overrides.0[&Action::Zl],
            vec![axis(Axis::LeftZ, Direction::Positive)]
        );
        assert_eq!(problems, vec![Problem::UnknownAction("nope".into())]);
    }

    #[test]
    fn action_ids_are_stable() {
        let ids: Vec<String> = Action::ALL.iter().map(|a| a.id()).collect();
        assert_eq!(ids[0], "a");
        assert_eq!(ids[11], "zr");
        assert_eq!(ids[15], "circle_pad_up");
        assert_eq!(ids[22], "c_stick_right");
    }
}
