//! Named input-layout presets (lotus#57): which physical inputs drive which 3DS action.
//!
//! A preset binds stable action IDs to portable binding descriptors: GPUI key names for the
//! keyboard (the strings `lotus_window.rs` matches today, not platform scancodes) and the
//! positional [`Button`]/[`Axis`] names for controllers (not backend IDs). Layers resolve like
//! worker settings (lotus#51): preset → global → card → a secondary's ephemeral overrides,
//! with the settings store choosing the winning layer per action ([`resolve`]).
//! See `docs/30-sessions-and-interface.md` "Input layouts (#57, #63)".

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
    /// A stored binding value that is not a list; ignored, so the layer below applies.
    NotAList { action: Action, value: Value },
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::UnknownAction(id) => write!(f, "unknown action '{id}' ignored"),
            Problem::InvalidBinding { action, binding } => {
                write!(f, "{}: unreadable binding {binding} ignored", action.id())
            }
            Problem::MissingAction(action) => {
                write!(f, "{}: not listed, builtin bindings used", action.id())
            }
            Problem::MissingPreset(name) => write!(f, "preset '{name}' not found, builtin used"),
            Problem::NotAList { action, value } => {
                write!(
                    f,
                    "{}: {value} is not a list of bindings, ignored",
                    action.id()
                )
            }
        }
    }
}

/// Why a preset file cannot be loaded at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    NotJson(String),
    /// Missing, zero or newer than [`VERSION`]; the file must not be rewritten.
    Version(Option<u64>),
    NoName,
    /// `bindings` is not an object, or an action's value is not a list. `[]` is a list
    /// (an explicit unbind); anything else would silently unbind the action.
    Malformed(String),
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
            LoadError::Malformed(what) => write!(f, "malformed preset: {what}"),
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
        // Trigger axes as ZL/ZR past the threshold, as Azahar's SDL default binds them
        // (`sdl_impl.cpp` `BuildButtonParamPackageForButton`), for backends that report
        // triggers as axes rather than as `LeftTrigger`/`RightTrigger` buttons.
        for (action, a, d) in [
            (Action::Zl, Axis::LeftZ, Positive),
            (Action::Zr, Axis::RightZ, Positive),
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
    /// builtin bindings. A wrong version, a non-preset, or a `bindings` value or known
    /// action's list of the wrong JSON type is refused.
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
            None => Map::new(),
            Some(other) => {
                return Err(LoadError::Malformed(format!(
                    "bindings is {other}, not an object"
                )))
            }
        };
        let (bindings, mut more) = parse_bindings(&listed)?;
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

type Bindings = BTreeMap<Action, Vec<Binding>>;

/// Parse `{"action_id": [binding, ...]}`, keeping what this build understands. A known
/// action whose value is not a list fails the whole map.
fn parse_bindings(map: &Map<String, Value>) -> Result<(Bindings, Vec<Problem>), LoadError> {
    let mut problems = Vec::new();
    let mut out = BTreeMap::new();
    let mut malformed = Vec::new();
    for (id, list) in map {
        let Ok(action) = serde_json::from_value::<Action>(Value::String(id.clone())) else {
            problems.push(Problem::UnknownAction(id.clone()));
            continue;
        };
        match parse_list(action, list) {
            Ok((parsed, mut more)) => {
                problems.append(&mut more);
                out.insert(action, parsed);
            }
            Err(_) => malformed.push(format!("bindings.{id} is {list}, not a list")),
        }
    }
    if !malformed.is_empty() {
        return Err(LoadError::Malformed(malformed.join("; ")));
    }
    Ok((out, problems))
}

/// Parse one action's stored binding list (a settings value). Unparseable entries are
/// dropped and reported; duplicates collapse. A value that is not a list is refused with
/// [`Problem::NotAList`] so callers never mistake it for `[]` (an explicit unbind).
pub fn parse_list(action: Action, value: &Value) -> Result<(Vec<Binding>, Vec<Problem>), Problem> {
    let Some(list) = value.as_array() else {
        return Err(Problem::NotAList {
            action,
            value: value.clone(),
        });
    };
    let mut problems = Vec::new();
    let mut parsed = Vec::new();
    for binding in list {
        match serde_json::from_value::<Binding>(binding.clone()) {
            Ok(b) if !parsed.contains(&b) => parsed.push(b),
            Ok(_) => {}
            Err(_) => problems.push(Problem::InvalidBinding {
                action,
                binding: binding.clone(),
            }),
        }
    }
    Ok((parsed, problems))
}

/// Where a resolved action's bindings came from, lowest first. The settings layers (lotus#51)
/// decide which override wins; this only records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// The selected preset.
    Preset,
    Global,
    Card,
    /// A secondary's ephemeral override.
    Session,
}

/// Effective per-action overrides: a replacement binding list (empty unbinds) and its layer.
pub type Overrides = BTreeMap<Action, (Vec<Binding>, Layer)>;

/// Presets shipped with Lotus; read-only. `default` is positional: 3DS A on the right face
/// button as on the 3DS, which is also where Nintendo pads print A, and the same positions
/// Azahar's own SDL default uses (`sdl_impl.cpp` `xinput_to_3ds_mapping`). `xbox-labels`
/// follows the printed Xbox labels instead (3DS A on the bottom face button). DualSense
/// buttons are printed with symbols, so position is their only meaning and `default` fits.
#[must_use]
pub fn builtins() -> Vec<Preset> {
    let default = Preset::builtin();
    let mut xbox = default.clone();
    xbox.name = "xbox-labels".into();
    for (action, button) in [
        (Action::A, Button::South),
        (Action::B, Button::East),
        (Action::X, Button::West),
        (Action::Y, Button::North),
    ] {
        let list = xbox.bindings.entry(action).or_default();
        list.retain(|b| !matches!(b, Binding::Button { .. }));
        list.push(pad(button));
    }
    vec![default, xbox]
}

/// Whether `name` belongs to a builtin preset, which cannot be saved over or deleted.
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    builtins().iter().any(|p| p.name == name)
}

/// Resolve a session's layout from the effective `preset` selection (`None`: [`BUILTIN`])
/// and the effective per-action `overrides`. A selected preset found in neither the builtins
/// nor `library` falls back to [`BUILTIN`] with a [`Problem::MissingPreset`].
#[must_use]
pub fn resolve(library: &[Preset], preset: Option<&str>, overrides: &Overrides) -> Resolved {
    let mut problems = Vec::new();
    let name = preset.unwrap_or(BUILTIN);
    let base = builtins()
        .into_iter()
        .chain(library.iter().cloned())
        .find(|p| p.name == name)
        .unwrap_or_else(|| {
            problems.push(Problem::MissingPreset(name.into()));
            Preset::builtin()
        });
    let mut bindings: BTreeMap<Action, (Vec<Binding>, Layer)> = base
        .bindings
        .iter()
        .map(|(a, list)| (*a, (list.clone(), Layer::Preset)))
        .collect();
    for (action, entry) in overrides {
        bindings.insert(*action, entry.clone());
    }
    Resolved {
        preset: base.name,
        bindings,
        problems,
    }
}

/// The layout one session uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Name of the preset the overrides apply to.
    pub preset: String,
    pub bindings: BTreeMap<Action, (Vec<Binding>, Layer)>,
    pub problems: Vec<Problem>,
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

    /// Actions with no binding on any device.
    #[must_use]
    pub fn unbound(&self) -> Vec<Action> {
        self.bindings
            .iter()
            .filter(|(_, (list, _))| list.is_empty())
            .map(|(action, _)| *action)
            .collect()
    }
}

/// Analog shaping for one 3DS stick.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct StickProfile {
    /// Radial dead zone in 0..0.99, applied on top of gilrs' own filter with Azahar's
    /// rescaling (`sdl_impl.cpp` `SDLAnalog::GetStatus`); Azahar's default is 0.
    pub deadzone: f32,
    /// Output scale; the result is clamped to the unit circle.
    pub sensitivity: f32,
    pub invert_y: bool,
}

impl Default for StickProfile {
    fn default() -> Self {
        Self {
            deadzone: 0.0,
            sensitivity: 1.0,
            invert_y: false,
        }
    }
}

/// Per-session shaping that is not a binding.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Profile {
    /// Circle Pad, C-stick.
    pub sticks: [StickProfile; 2],
    /// Axis travel that presses an axis-to-digital binding. Azahar binds ZL/ZR to the trigger
    /// axes at 0.5 (`sdl_impl.cpp` `BuildButtonParamPackageForButton`).
    pub threshold: f32,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            sticks: [StickProfile::default(); 2],
            threshold: 0.5,
        }
    }
}

/// Stick direction actions per 3DS stick, in `[up, down, left, right]` order.
const STICK_ACTIONS: [[Action; 4]; 2] = [
    [
        Action::CirclePadUp,
        Action::CirclePadDown,
        Action::CirclePadLeft,
        Action::CirclePadRight,
    ],
    [
        Action::CStickUp,
        Action::CStickDown,
        Action::CStickLeft,
        Action::CStickRight,
    ],
];

/// Offset a digitally pressed stick direction adds, in [`STICK_ACTIONS`] order.
const DIRECTION_DELTA: [[i8; 2]; 4] = [[0, 1], [0, -1], [-1, 0], [1, 0]];

/// What one digital input does: button bits plus a push on each 3DS stick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Press {
    bits: u32,
    sticks: [[i8; 2]; 2],
}

impl Press {
    fn add(&mut self, action: Action) {
        if let Some(bit) = action.bit() {
            self.bits |= bit;
            return;
        }
        for (stick, actions) in STICK_ACTIONS.iter().enumerate() {
            if let Some(d) = actions.iter().position(|a| *a == action) {
                self.sticks[stick][0] += DIRECTION_DELTA[d][0];
                self.sticks[stick][1] += DIRECTION_DELTA[d][1];
            }
        }
    }

    fn merge(&mut self, other: &Press) {
        self.bits |= other.bits;
        for stick in 0..2 {
            for c in 0..2 {
                self.sticks[stick][c] =
                    self.sticks[stick][c].saturating_add(other.sticks[stick][c]);
            }
        }
    }
}

/// Guest buttons and the two sticks one source contributes.
pub type Output = (u32, [[f32; 2]; 2]);

/// A [`Resolved`] layout compiled for the router. Controller lookups are fixed-size tables
/// indexed by [`Button::index`], so evaluating a sample allocates nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    buttons: [Press; 17],
    /// Analog source per 3DS stick and component (x, y): axis and sign.
    analog: [[Option<(Axis, f32)>; 2]; 2],
    /// Axis bindings that act digitally past the threshold.
    axis_presses: Vec<(Axis, Direction, Press)>,
    keys: BTreeMap<String, Press>,
    profile: Profile,
}

impl Default for Mapping {
    fn default() -> Self {
        Self::new(&resolve(&[], None, &Overrides::new()), Profile::default())
    }
}

fn sign(direction: Direction) -> f32 {
    match direction {
        Direction::Positive => 1.0,
        Direction::Negative => -1.0,
    }
}

impl Mapping {
    /// An axis bound to a stick's right (x) or up (y) action drives that component in
    /// analog, as does one bound to left/down with the sign flipped; the first such binding
    /// wins. Every other axis binding is digital, pressed past [`Profile::threshold`].
    #[must_use]
    pub fn new(layout: &Resolved, profile: Profile) -> Self {
        let list = |action: Action| -> &[Binding] {
            layout
                .bindings
                .get(&action)
                .map_or(&[], |(list, _)| list.as_slice())
        };
        let first_axis = |action: Action, flip: f32| {
            list(action).iter().find_map(|b| match b {
                Binding::Axis { axis, direction } => Some((*axis, sign(*direction) * flip)),
                _ => None,
            })
        };
        let mut analog = [[None; 2]; 2];
        for (stick, [up, down, left, right]) in STICK_ACTIONS.iter().enumerate() {
            analog[stick][0] = first_axis(*right, 1.0).or_else(|| first_axis(*left, -1.0));
            analog[stick][1] = first_axis(*up, 1.0).or_else(|| first_axis(*down, -1.0));
        }
        let is_analog = |action: Action, axis: Axis| {
            STICK_ACTIONS.iter().enumerate().any(|(stick, actions)| {
                let component = match actions.iter().position(|a| *a == action) {
                    Some(0 | 1) => 1,
                    Some(_) => 0,
                    None => return false,
                };
                analog[stick][component].is_some_and(|(a, _)| a == axis)
            })
        };
        let mut buttons = [Press::default(); 17];
        let mut axis_presses: Vec<(Axis, Direction, Press)> = Vec::new();
        let mut keys: BTreeMap<String, Press> = BTreeMap::new();
        for (action, (bindings, _)) in &layout.bindings {
            for binding in bindings {
                match binding {
                    Binding::Keyboard { key } => keys.entry(key.clone()).or_default().add(*action),
                    Binding::Button { button } => buttons[button.index()].add(*action),
                    Binding::Axis { axis, direction } => {
                        if is_analog(*action, *axis) {
                            continue;
                        }
                        let index = axis_presses
                            .iter()
                            .position(|(a, d, _)| a == axis && d == direction)
                            .unwrap_or_else(|| {
                                axis_presses.push((*axis, *direction, Press::default()));
                                axis_presses.len() - 1
                            });
                        axis_presses[index].2.add(*action);
                    }
                }
            }
        }
        Self {
            buttons,
            analog,
            axis_presses,
            keys,
            profile,
        }
    }

    #[must_use]
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// A controller's contribution. `held` and `axis` already leave out suppressed inputs.
    #[must_use]
    pub fn controller(&self, held: impl Fn(Button) -> bool, axis: impl Fn(Axis) -> f32) -> Output {
        let mut digital = Press::default();
        for button in Button::ALL {
            if held(button) {
                digital.merge(&self.buttons[button.index()]);
            }
        }
        for (a, direction, press) in &self.axis_presses {
            if axis(*a) * sign(*direction) > self.profile.threshold {
                digital.merge(press);
            }
        }
        let mut sticks = [[0.0f32; 2]; 2];
        for (stick, out) in sticks.iter_mut().enumerate() {
            let raw = self.analog[stick].map(|source| source.map_or(0.0, |(a, s)| axis(a) * s));
            *out = shape(raw, self.profile.sticks[stick]);
            for (value, push) in out.iter_mut().zip(digital.sticks[stick]) {
                *value = (*value + f32::from(push)).clamp(-1.0, 1.0);
            }
        }
        (digital.bits, sticks)
    }

    /// The keyboard's contribution from the keys it holds.
    #[must_use]
    pub fn keyboard<'a>(&self, keys: impl IntoIterator<Item = &'a String>) -> Output {
        let mut digital = Press::default();
        for key in keys {
            if let Some(press) = self.keys.get(key) {
                digital.merge(press);
            }
        }
        (
            digital.bits,
            digital
                .sticks
                .map(|s| s.map(|c| f32::from(c).clamp(-1.0, 1.0))),
        )
    }
}

/// Radial dead zone with Azahar's rescaling, then sensitivity, unit-circle clamp, inversion.
/// Digital directions bypass this, so the dead zone never eats a button-driven direction.
fn shape([x, y]: [f32; 2], profile: StickProfile) -> [f32; 2] {
    let r = x.hypot(y);
    let deadzone = profile.deadzone.clamp(0.0, 0.99);
    if r <= deadzone || r == 0.0 {
        return [0.0, 0.0];
    }
    let scale = ((r - deadzone) / (1.0 - deadzone) * profile.sensitivity).min(1.0) / r;
    [
        x * scale,
        if profile.invert_y {
            -y * scale
        } else {
            y * scale
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Resolved {
        resolve(&[], None, &Overrides::new())
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
    fn import_refuses_lists_of_the_wrong_type() {
        for bad in ["null", "{}", r#""z""#, "true", "1"] {
            let json = format!(r#"{{"version":1,"name":"n","bindings":{{"a":{bad}}}}}"#);
            assert!(
                matches!(Preset::import(&json), Err(LoadError::Malformed(ref m)) if m.contains("bindings.a")),
                "bindings.a = {bad}"
            );
            let json = format!(r#"{{"version":1,"name":"n","bindings":{bad}}}"#);
            if bad != "{}" {
                assert!(
                    matches!(Preset::import(&json), Err(LoadError::Malformed(_))),
                    "bindings = {bad}"
                );
            }
            let value: Value = serde_json::from_str(bad).unwrap();
            assert_eq!(
                parse_list(Action::A, &value),
                Err(Problem::NotAList {
                    action: Action::A,
                    value: value.clone()
                })
            );
        }
        // `[]` still unbinds; an unknown action keeps forward compatibility whatever its type;
        // a mixed list keeps what parses.
        let json = r#"{"version":1,"name":"n","bindings":{"a":[],"teleport":{},
            "b":[{"device":"keyboard","key":"k"},7,{"device":"keyboard"}]}}"#;
        let (preset, problems) = Preset::import(json).unwrap();
        assert_eq!(preset.bindings[&Action::A], vec![]);
        assert_eq!(preset.bindings[&Action::B], vec![key("k")]);
        assert!(problems.contains(&Problem::UnknownAction("teleport".into())));
        let invalid = problems
            .iter()
            .filter(|p| {
                matches!(
                    p,
                    Problem::InvalidBinding {
                        action: Action::B,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(invalid, 2, "{problems:?}");
        assert_eq!(
            Problem::UnknownAction("teleport".into()).to_string(),
            "unknown action 'teleport' ignored"
        );
        // No bindings at all: every action takes the builtin.
        let (preset, _) = Preset::import(r#"{"version":1,"name":"n"}"#).unwrap();
        assert_eq!(preset.bindings, Preset::builtin().bindings);
    }

    fn over(entries: &[(Action, Vec<Binding>, Layer)]) -> Overrides {
        entries
            .iter()
            .map(|(a, list, layer)| (*a, (list.clone(), *layer)))
            .collect()
    }

    #[test]
    fn overrides_replace_whole_actions_and_empty_unbinds() {
        let overrides = over(&[
            (Action::A, vec![key("j")], Layer::Global),
            (Action::B, vec![key("l")], Layer::Session),
            (Action::Start, vec![], Layer::Card),
        ]);
        let resolved = resolve(&[], None, &overrides);
        assert_eq!(resolved.bindings[&Action::A].1, Layer::Global);
        assert_eq!(resolved.key_bits("j"), bits::A);
        assert_eq!(resolved.key_bits("z"), 0, "A replaced, not added to");
        assert_eq!(
            resolved.bindings[&Action::B],
            (vec![key("l")], Layer::Session)
        );
        assert_eq!(resolved.key_bits("enter"), 0, "unbound by an empty list");
        assert_eq!(resolved.unbound(), vec![Action::Start]);
        assert_eq!(resolved.bindings[&Action::Y].1, Layer::Preset);
    }

    #[test]
    fn presets_resolve_from_builtins_then_library_and_missing_falls_back() {
        let mut lefty = Preset::builtin();
        lefty.name = "lefty".into();
        lefty.bindings.insert(Action::A, vec![key("m")]);
        let library = [lefty];
        let resolved = resolve(&library, Some("lefty"), &Overrides::new());
        assert_eq!(resolved.preset, "lefty");
        assert_eq!(resolved.key_bits("m"), bits::A);
        assert!(resolve(&library, Some("xbox-labels"), &Overrides::new())
            .problems
            .is_empty());

        let resolved = resolve(&[], Some("deleted"), &Overrides::new());
        assert_eq!(resolved.preset, BUILTIN);
        assert_eq!(
            resolved.problems,
            vec![Problem::MissingPreset("deleted".into())]
        );
        assert_eq!(resolved.key_bits("z"), bits::A);
        assert!(is_builtin("default") && is_builtin("xbox-labels") && !is_builtin("lefty"));
    }

    #[test]
    fn conflicts_are_reported_and_press_both() {
        let overrides = over(&[(Action::B, vec![key("z")], Layer::Card)]);
        let resolved = resolve(&[], None, &overrides);
        assert_eq!(
            resolved.conflicts(),
            vec![(key("z"), vec![Action::A, Action::B])]
        );
        assert_eq!(resolved.key_bits("z"), bits::A | bits::B);
        let mapping = Mapping::new(&resolved, Profile::default());
        assert_eq!(mapping.keyboard([&"z".to_string()]).0, bits::A | bits::B);
    }

    #[test]
    fn parse_list_reports_invalid_entries() {
        let value = serde_json::json!([
            {"device":"axis","axis":"left_z","direction":"positive"},
            {"device":"axis","axis":"left_z","direction":"positive"},
            {"device":"button","button":"paddle_9"}
        ]);
        let (list, problems) = parse_list(Action::Zl, &value).unwrap();
        assert_eq!(list, vec![axis(Axis::LeftZ, Direction::Positive)]);
        assert!(matches!(
            problems.as_slice(),
            [Problem::InvalidBinding {
                action: Action::Zl,
                ..
            }]
        ));
    }

    /// A pad's state for [`Mapping::controller`].
    fn eval(mapping: &Mapping, held: &[Button], axes: &[(Axis, f32)]) -> Output {
        mapping.controller(
            |b| held.contains(&b),
            |a| axes.iter().find(|(x, _)| *x == a).map_or(0.0, |(_, v)| *v),
        )
    }

    fn mapping(preset: &str) -> Mapping {
        Mapping::new(
            &resolve(&[], Some(preset), &Overrides::new()),
            Profile::default(),
        )
    }

    /// Sample sequences per pad family. gilrs reports positions (SDL naming), so each
    /// family's printed labels are named here only to say which physical button was pressed.
    #[test]
    fn pad_family_fixtures() {
        let positional = mapping("default");
        let xbox = mapping("xbox-labels");
        // Xbox pad, printed A (bottom = South): positional gives 3DS B, xbox-labels 3DS A.
        assert_eq!(eval(&positional, &[Button::South], &[]).0, bits::B);
        assert_eq!(eval(&xbox, &[Button::South], &[]).0, bits::A);
        // Xbox printed Y (top = North) -> 3DS X positionally, 3DS Y by label.
        assert_eq!(eval(&positional, &[Button::North], &[]).0, bits::X);
        assert_eq!(eval(&xbox, &[Button::North], &[]).0, bits::Y);
        // DualSense cross/circle/square/triangle = South/East/West/North: positional only.
        let ds = [Button::South, Button::East, Button::West, Button::North];
        let got: Vec<u32> = ds.iter().map(|b| eval(&positional, &[*b], &[]).0).collect();
        assert_eq!(got, vec![bits::B, bits::A, bits::Y, bits::X]);
        // Nintendo layout: printed A is on the right (East), as on the 3DS.
        assert_eq!(eval(&positional, &[Button::East], &[]).0, bits::A);
        // Triggers as buttons (XInput/WGI) and as axes (backends with LeftZ/RightZ).
        assert_eq!(eval(&positional, &[Button::LeftTrigger], &[]).0, bits::ZL);
        assert_eq!(eval(&positional, &[], &[(Axis::RightZ, 0.6)]).0, bits::ZR);
        assert_eq!(eval(&positional, &[], &[(Axis::RightZ, 0.4)]).0, 0);
        // Sticks pass through unchanged by default.
        let (_, sticks) = eval(
            &positional,
            &[],
            &[(Axis::LeftX, -0.5), (Axis::RightY, 0.25)],
        );
        assert_eq!(sticks, [[-0.5, 0.0], [0.0, 0.25]]);
        // A limited pad (no right stick, no triggers) yields nothing for missing controls.
        let (bits_, sticks) = eval(&positional, &[Button::DpadUp], &[]);
        assert_eq!((bits_, sticks[1]), (bits::UP, [0.0, 0.0]));
    }

    #[test]
    fn stick_deadzone_sensitivity_and_inversion() {
        let mut profile = Profile::default();
        profile.sticks[0] = StickProfile {
            deadzone: 0.2,
            sensitivity: 1.0,
            invert_y: true,
        };
        profile.sticks[1].sensitivity = 2.0;
        let m = Mapping::new(&resolve(&[], None, &Overrides::new()), profile);
        let circle = |x: f32, y: f32| eval(&m, &[], &[(Axis::LeftX, x), (Axis::LeftY, y)]).1[0];
        assert_eq!(circle(0.15, 0.1), [0.0, 0.0], "inside the radial dead zone");
        // Azahar: x / r * (r - dz) / (1 - dz); r = 0.6 -> 0.5.
        let [x, y] = circle(0.6, 0.0);
        assert!((x - 0.5).abs() < 1e-6 && y == 0.0);
        let [_, y] = circle(0.0, 1.0);
        assert!((y + 1.0).abs() < 1e-6, "inverted");
        let cstick = |x: f32| eval(&m, &[], &[(Axis::RightX, x)]).1[1];
        assert_eq!(cstick(0.3), [0.6, 0.0]);
        assert_eq!(cstick(0.8), [1.0, 0.0], "clamped to the unit circle");
    }

    #[test]
    fn deadzone_never_eats_digital_inputs() {
        let mut profile = Profile::default();
        profile.sticks[0].deadzone = 0.9;
        let overrides = over(&[
            (
                Action::CirclePadUp,
                vec![pad(Button::DpadUp), key("i")],
                Layer::Global,
            ),
            (
                Action::Zl,
                vec![axis(Axis::LeftX, Direction::Negative)],
                Layer::Global,
            ),
        ]);
        let m = Mapping::new(&resolve(&[], None, &overrides), profile);
        // D-pad up drives the circle pad fully, whatever the dead zone.
        assert_eq!(eval(&m, &[Button::DpadUp], &[]).1[0], [0.0, 1.0]);
        assert_eq!(m.keyboard([&"i".to_string()]).1[0], [0.0, 1.0]);
        // The left stick's x is both the circle pad's analog x and a digital ZL; the
        // threshold, not the stick dead zone, decides the press.
        let (b, sticks) = eval(&m, &[], &[(Axis::LeftX, -0.6)]);
        assert_eq!(b, bits::ZL);
        assert_eq!(sticks[0], [0.0, 0.0], "analog x still inside the dead zone");
    }

    #[test]
    fn axis_and_button_rebinds() {
        // Right stick drives the circle pad (axes moved, sign kept); left stick unbound.
        let overrides = over(&[
            (
                Action::CirclePadRight,
                vec![axis(Axis::RightX, Direction::Positive)],
                Layer::Card,
            ),
            (
                Action::CirclePadLeft,
                vec![axis(Axis::RightX, Direction::Negative)],
                Layer::Card,
            ),
            (
                Action::CirclePadUp,
                vec![axis(Axis::RightY, Direction::Negative)],
                Layer::Card,
            ),
            (Action::CirclePadDown, vec![], Layer::Card),
            (Action::CStickRight, vec![], Layer::Card),
            (Action::CStickLeft, vec![], Layer::Card),
            (Action::CStickUp, vec![], Layer::Card),
            (Action::CStickDown, vec![], Layer::Card),
            (Action::Home, vec![pad(Button::LeftThumb)], Layer::Card),
        ]);
        let m = Mapping::new(&resolve(&[], None, &overrides), Profile::default());
        let (b, sticks) = eval(
            &m,
            &[Button::LeftThumb],
            &[(Axis::RightX, 0.5), (Axis::RightY, 0.5), (Axis::LeftX, 1.0)],
        );
        assert_eq!(b, bits::HOME);
        // Up bound to RightY negative: pushing the stick up (positive) points the pad down.
        assert_eq!(sticks, [[0.5, -0.5], [0.0, 0.0]]);
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
