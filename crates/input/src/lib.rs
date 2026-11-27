//! Controller input: a virtual GameCube pad fed by keyboard and gamepads, with persistent, fully
//! remappable bindings and the button-glyph set the player prefers.
//!
//! Sources are named strings so the configuration file stays readable and independent of the windowing
//! library: keyboard keys use winit's `KeyCode` names (`KeyZ`, `ArrowUp`, `Enter`), gamepad inputs use SDL's
//! names (`a`, `leftshoulder`, `dpup`, `leftx+`, `lefttrigger`, ...).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use fepor_runtime::si::{button, PadState};
use serde::{Deserialize, Serialize};

mod controls;
pub mod gamepad;

pub use controls::{Controls, DeviceInfo};

/// Everything a GameCube controller can do.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Action {
    A,
    B,
    X,
    Y,
    Z,
    Start,
    L,
    R,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    StickUp,
    StickDown,
    StickLeft,
    StickRight,
    CUp,
    CDown,
    CLeft,
    CRight,
}

impl Action {
    pub const ALL: [Action; 20] = [
        Action::A,
        Action::B,
        Action::X,
        Action::Y,
        Action::Z,
        Action::Start,
        Action::L,
        Action::R,
        Action::DpadUp,
        Action::DpadDown,
        Action::DpadLeft,
        Action::DpadRight,
        Action::StickUp,
        Action::StickDown,
        Action::StickLeft,
        Action::StickRight,
        Action::CUp,
        Action::CDown,
        Action::CLeft,
        Action::CRight,
    ];

    /// The name printed on a GameCube controller.
    pub fn label(self) -> &'static str {
        match self {
            Action::A => "A",
            Action::B => "B",
            Action::X => "X",
            Action::Y => "Y",
            Action::Z => "Z",
            Action::Start => "Start",
            Action::L => "L",
            Action::R => "R",
            Action::DpadUp => "D-pad up",
            Action::DpadDown => "D-pad down",
            Action::DpadLeft => "D-pad left",
            Action::DpadRight => "D-pad right",
            Action::StickUp => "Stick up",
            Action::StickDown => "Stick down",
            Action::StickLeft => "Stick left",
            Action::StickRight => "Stick right",
            Action::CUp => "C-stick up",
            Action::CDown => "C-stick down",
            Action::CLeft => "C-stick left",
            Action::CRight => "C-stick right",
        }
    }

    /// A few characters for a tag on a drawing of a pad.
    pub fn short(self) -> &'static str {
        match self {
            Action::A | Action::B | Action::X | Action::Y | Action::Z | Action::L | Action::R => {
                self.label()
            }
            Action::Start => "Start",
            Action::DpadUp => "D-Up",
            Action::DpadDown => "D-Dn",
            Action::DpadLeft => "D-Lt",
            Action::DpadRight => "D-Rt",
            Action::StickUp => "S-Up",
            Action::StickDown => "S-Dn",
            Action::StickLeft => "S-Lt",
            Action::StickRight => "S-Rt",
            Action::CUp => "C-Up",
            Action::CDown => "C-Dn",
            Action::CLeft => "C-Lt",
            Action::CRight => "C-Rt",
        }
    }
}

/// Which family of button icons to show for GameCube buttons the player has mapped.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum GlyphSet {
    #[default]
    GameCube,
    Xbox,
    Nintendo,
    PlayStation,
}

impl GlyphSet {
    /// What the physical button bound to `source` is called on this family of controllers (gamepad sources).
    pub fn source_label(self, source: &str) -> String {
        let face = |xbox: &'static str, nin: &'static str, ps: &'static str| match self {
            GlyphSet::Xbox | GlyphSet::GameCube => xbox,
            GlyphSet::Nintendo => nin,
            GlyphSet::PlayStation => ps,
        };
        // SDL names buttons by position (a = south, b = east, x = west, y = north)
        match source {
            "a" => face("A", "B", "Cross").into(),
            "b" => face("B", "A", "Circle").into(),
            "x" => face("X", "Y", "Square").into(),
            "y" => face("Y", "X", "Triangle").into(),
            "back" => face("View", "-", "Share").into(),
            "start" => face("Menu", "+", "Options").into(),
            "leftshoulder" => face("LB", "L", "L1").into(),
            "rightshoulder" => face("RB", "R", "R1").into(),
            "lefttrigger" => face("LT", "ZL", "L2").into(),
            "righttrigger" => face("RT", "ZR", "R2").into(),
            "leftstick" => face("LS", "L3", "L3").into(),
            "rightstick" => face("RS", "R3", "R3").into(),
            other => other.to_string(),
        }
    }

    /// The label of a GameCube action on the GameCube pad (the icons the game itself draws).
    pub fn action_label(self, a: Action) -> &'static str {
        a.label()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bindings {
    /// keyboard key name -> action
    pub keyboard: BTreeMap<String, Action>,
    /// gamepad input name -> action
    pub gamepad: BTreeMap<String, Action>,
}

impl Default for Bindings {
    fn default() -> Self {
        let kb = [
            ("KeyZ", Action::A),
            ("KeyX", Action::B),
            ("KeyC", Action::X),
            ("KeyV", Action::Y),
            ("Space", Action::Z),
            ("Enter", Action::Start),
            ("KeyQ", Action::L),
            ("KeyE", Action::R),
            ("KeyW", Action::DpadUp),
            ("KeyS", Action::DpadDown),
            ("KeyA", Action::DpadLeft),
            ("KeyD", Action::DpadRight),
            ("ArrowUp", Action::StickUp),
            ("ArrowDown", Action::StickDown),
            ("ArrowLeft", Action::StickLeft),
            ("ArrowRight", Action::StickRight),
            ("KeyI", Action::CUp),
            ("KeyK", Action::CDown),
            ("KeyJ", Action::CLeft),
            ("KeyL", Action::CRight),
        ];
        // SDL positions: south = GC A, west = GC B, east = GC X, north = GC Y
        let pad = [
            ("a", Action::A),
            ("x", Action::B),
            ("b", Action::X),
            ("y", Action::Y),
            ("rightshoulder", Action::Z),
            ("start", Action::Start),
            ("lefttrigger", Action::L),
            ("righttrigger", Action::R),
            ("dpup", Action::DpadUp),
            ("dpdown", Action::DpadDown),
            ("dpleft", Action::DpadLeft),
            ("dpright", Action::DpadRight),
            ("lefty-", Action::StickUp),
            ("lefty+", Action::StickDown),
            ("leftx-", Action::StickLeft),
            ("leftx+", Action::StickRight),
            ("righty-", Action::CUp),
            ("righty+", Action::CDown),
            ("rightx-", Action::CLeft),
            ("rightx+", Action::CRight),
        ];
        Bindings {
            keyboard: kb.iter().map(|(k, a)| (k.to_string(), *a)).collect(),
            gamepad: pad.iter().map(|(k, a)| (k.to_string(), *a)).collect(),
        }
    }
}

/// The gamepad bindings and button names of one controller model, picked automatically when a pad with the
/// model's GUID is plugged in (`[profiles.<guid>]` in `input.toml`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// The name the pad reported when the profile was last used.
    pub name: String,
    pub glyphs: GlyphSet,
    /// gamepad input name -> action
    pub gamepad: BTreeMap<String, Action>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    /// The button-name family of new profiles, and of the gamepad while none is plugged in.
    pub glyphs: GlyphSet,
    /// Analog values below this (0..1) read as zero.
    pub deadzone: f32,
    /// `bindings.keyboard` is the keyboard's; `bindings.gamepad` is what a controller model gets the first
    /// time it is seen, and what applies while no pad is plugged in (the only gamepad bindings files from
    /// before profiles have).
    pub bindings: Bindings,
    /// One profile per controller model, keyed by its SDL GUID.
    pub profiles: BTreeMap<String, Profile>,
}

impl Default for InputConfig {
    fn default() -> Self {
        InputConfig {
            glyphs: GlyphSet::default(),
            deadzone: 0.15,
            bindings: Bindings::default(),
            profiles: BTreeMap::new(),
        }
    }
}

impl InputConfig {
    /// Load `path`, writing the defaults there when it does not exist yet.
    pub fn load_or_create(path: &Path) -> InputConfig {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("input: {}: {e}; using the defaults", path.display());
                    InputConfig::default()
                }
            },
            Err(_) => {
                let mut c = InputConfig::default();
                // RecompLauncher tells which controller is plugged in: start with its button names
                c.glyphs = match std::env::var("RECOMPLAUNCHER_PAD").as_deref() {
                    Ok("xbox") => GlyphSet::Xbox,
                    Ok("playstation") => GlyphSet::PlayStation,
                    _ => c.glyphs,
                };
                let _ = c.save(path);
                c
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            path,
            toml::to_string_pretty(self).map_err(std::io::Error::other)?,
        )
    }

    /// Make sure the controller model `guid` has a profile: a new one starts from the shared gamepad
    /// bindings and button names, so a pad of a model never seen before behaves as the single pad of
    /// earlier versions did. The profile remembers `name`. Returns whether the profile is new.
    pub fn ensure_profile(&mut self, guid: &str, name: &str) -> bool {
        if let Some(p) = self.profiles.get_mut(guid) {
            if !name.is_empty() {
                p.name = name.to_string();
            }
            return false;
        }
        self.profiles.insert(
            guid.to_string(),
            Profile {
                name: name.to_string(),
                glyphs: self.glyphs,
                gamepad: self.bindings.gamepad.clone(),
            },
        );
        true
    }

    /// The gamepad bindings of profile `profile`, or the shared ones when there is none.
    pub fn gamepad_map(&self, profile: Option<&str>) -> &BTreeMap<String, Action> {
        match profile.and_then(|g| self.profiles.get(g)) {
            Some(p) => &p.gamepad,
            None => &self.bindings.gamepad,
        }
    }

    fn gamepad_map_mut(&mut self, profile: Option<&str>) -> &mut BTreeMap<String, Action> {
        match profile.filter(|g| self.profiles.contains_key(*g)) {
            Some(g) => &mut self.profiles.get_mut(g).unwrap().gamepad,
            None => &mut self.bindings.gamepad,
        }
    }

    /// The button-name family of profile `profile`, or the shared one when there is none.
    pub fn glyphs_of(&self, profile: Option<&str>) -> GlyphSet {
        profile
            .and_then(|g| self.profiles.get(g))
            .map_or(self.glyphs, |p| p.glyphs)
    }

    pub fn set_glyphs_of(&mut self, profile: Option<&str>, glyphs: GlyphSet) {
        match profile.and_then(|g| self.profiles.get_mut(g)) {
            Some(p) => p.glyphs = glyphs,
            None => self.glyphs = glyphs,
        }
    }

    /// Bind `source` (a keyboard key when `keyboard`, otherwise a gamepad input of `profile`) to `action`,
    /// replacing any earlier binding of that source and any other source the same device had for the action.
    pub fn bind(&mut self, keyboard: bool, profile: Option<&str>, source: &str, action: Action) {
        let map = if keyboard {
            &mut self.bindings.keyboard
        } else {
            self.gamepad_map_mut(profile)
        };
        map.retain(|_, a| *a != action);
        map.insert(source.to_string(), action);
    }

    /// Leave `source` unbound. Returns whether it was bound.
    pub fn unbind(&mut self, keyboard: bool, profile: Option<&str>, source: &str) -> bool {
        let map = if keyboard {
            &mut self.bindings.keyboard
        } else {
            self.gamepad_map_mut(profile)
        };
        map.remove(source).is_some()
    }

    /// Sources bound to `action` on the keyboard or on the gamepad of `profile`.
    pub fn sources_of(&self, keyboard: bool, profile: Option<&str>, action: Action) -> Vec<&str> {
        let map = if keyboard {
            &self.bindings.keyboard
        } else {
            self.gamepad_map(profile)
        };
        map.iter()
            .filter(|(_, a)| **a == action)
            .map(|(s, _)| s.as_str())
            .collect()
    }
}

/// Live state of every bound source.
#[derive(Default)]
pub struct InputState {
    /// source name (prefixed `kb:` or `pad:`) -> strength 0..1
    pub(crate) sources: HashMap<String, f32>,
}

const STICK_RANGE: f32 = 100.0;

impl InputState {
    /// Report the strength of a source: 1.0 / 0.0 for keys and buttons, the axis value for analog inputs.
    pub fn set(&mut self, keyboard: bool, source: &str, value: f32) {
        let key = format!("{}{source}", if keyboard { "kb:" } else { "pad:" });
        if value > 0.0 {
            self.sources.insert(key, value.min(1.0));
        } else {
            self.sources.remove(&key);
        }
    }

    pub fn release_all(&mut self, keyboard: bool) {
        let prefix = if keyboard { "kb:" } else { "pad:" };
        self.sources.retain(|k, _| !k.starts_with(prefix));
    }

    /// The strength of `action` from the keyboard bindings and the gamepad bindings of `profile`.
    fn strength(&self, cfg: &InputConfig, profile: Option<&str>, action: Action) -> f32 {
        let mut best: f32 = 0.0;
        for (keyboard, map) in [
            (true, &cfg.bindings.keyboard),
            (false, cfg.gamepad_map(profile)),
        ] {
            for (src, a) in map {
                if *a != action {
                    continue;
                }
                let key = format!("{}{src}", if keyboard { "kb:" } else { "pad:" });
                if let Some(&v) = self.sources.get(&key) {
                    // deadzone, rescaled so the stick still reaches full range
                    let v = if v < cfg.deadzone {
                        0.0
                    } else {
                        (v - cfg.deadzone) / (1.0 - cfg.deadzone)
                    };
                    best = best.max(v);
                }
            }
        }
        best
    }

    /// The controller state the GameCube sees: the keyboard and the gamepad bindings of `profile` (the
    /// keyboard's keys and the active pad's inputs share one state).
    pub fn pad(&self, cfg: &InputConfig, profile: Option<&str>) -> PadState {
        let s = |a| self.strength(cfg, profile, a);
        let on = |a| s(a) > 0.5;
        let mut b = 0u16;
        for (a, bit) in [
            (Action::A, button::A),
            (Action::B, button::B),
            (Action::X, button::X),
            (Action::Y, button::Y),
            (Action::Z, button::Z),
            (Action::Start, button::START),
            (Action::DpadUp, button::UP),
            (Action::DpadDown, button::DOWN),
            (Action::DpadLeft, button::LEFT),
            (Action::DpadRight, button::RIGHT),
        ] {
            if on(a) {
                b |= bit;
            }
        }
        // the shoulder buttons click only near the end of their travel
        let (l, r) = (s(Action::L), s(Action::R));
        if l > 0.85 {
            b |= button::L;
        }
        if r > 0.85 {
            b |= button::R;
        }
        let axis = |neg: Action, pos: Action| {
            (128.0 + (s(pos) - s(neg)) * STICK_RANGE)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        PadState {
            buttons: b,
            stick: (
                axis(Action::StickLeft, Action::StickRight),
                axis(Action::StickDown, Action::StickUp),
            ),
            c_stick: (
                axis(Action::CLeft, Action::CRight),
                axis(Action::CDown, Action::CUp),
            ),
            triggers: ((l * 255.0) as u8, (r * 255.0) as u8),
        }
    }
}

/// Interactive remapping: asks for each action in turn and takes the next input as its binding.
pub struct Remapper {
    keyboard: bool,
    /// The gamepad profile the bindings go to (the shared bindings when `None`).
    profile: Option<String>,
    queue: Vec<Action>,
    next: usize,
}

pub enum RemapStep {
    /// Waiting for an input for this action.
    Ask(Action),
    /// Every action was visited; the configuration should be saved.
    Done,
}

impl Remapper {
    pub fn new(keyboard: bool, profile: Option<String>) -> Remapper {
        Remapper {
            keyboard,
            profile,
            queue: Action::ALL.to_vec(),
            next: 0,
        }
    }

    /// Remap `action` only.
    pub fn single(keyboard: bool, profile: Option<String>, action: Action) -> Remapper {
        Remapper {
            keyboard,
            profile,
            queue: vec![action],
            next: 0,
        }
    }

    pub fn keyboard(&self) -> bool {
        self.keyboard
    }

    pub fn current(&self) -> RemapStep {
        match self.queue.get(self.next) {
            Some(a) => RemapStep::Ask(*a),
            None => RemapStep::Done,
        }
    }

    /// An input arrived; `source` is `None` to skip the action (keep its current binding).
    pub fn feed(&mut self, cfg: &mut InputConfig, source: Option<&str>) -> RemapStep {
        if let (Some(a), Some(src)) = (self.queue.get(self.next).copied(), source) {
            cfg.bind(self.keyboard, self.profile.as_deref(), src, a);
        }
        self.next += 1;
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_and_defaults_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.toml");
        let mut c = InputConfig::load_or_create(&path);
        assert!(path.exists());
        c.bind(true, None, "KeyF", Action::A);
        c.glyphs = GlyphSet::PlayStation;
        c.save(&path).unwrap();
        let back = InputConfig::load_or_create(&path);
        assert_eq!(back, c);
        assert_eq!(back.sources_of(true, None, Action::A), vec!["KeyF"]);
    }

    #[test]
    fn digital_and_analog_sources_make_a_pad() {
        let cfg = InputConfig::default();
        let mut st = InputState::default();
        st.set(true, "KeyZ", 1.0);
        st.set(true, "ArrowRight", 1.0);
        st.set(false, "lefttrigger", 0.6);
        let p = st.pad(&cfg, None);
        assert_eq!(p.buttons & button::A, button::A);
        assert_eq!(p.stick.0, 228);
        assert_eq!(p.stick.1, 128);
        // a half-pressed trigger moves the analog value without clicking
        assert_eq!(p.buttons & button::L, 0);
        assert!(p.triggers.0 > 100 && p.triggers.0 < 160, "{}", p.triggers.0);
        st.set(false, "lefttrigger", 1.0);
        assert_eq!(st.pad(&cfg, None).buttons & button::L, button::L);
    }

    #[test]
    fn deadzone_hides_small_axis_values() {
        let cfg = InputConfig::default();
        let mut st = InputState::default();
        st.set(false, "leftx+", 0.1);
        assert_eq!(st.pad(&cfg, None).stick, (128, 128));
    }

    #[test]
    fn remapping_replaces_bindings_and_a_skip_keeps_them() {
        let mut cfg = InputConfig::default();
        let mut r = Remapper::new(false, None);
        // A <- "y" pad button, B skipped
        assert!(matches!(r.current(), RemapStep::Ask(Action::A)));
        r.feed(&mut cfg, Some("y"));
        r.feed(&mut cfg, None);
        assert_eq!(cfg.bindings.gamepad.get("y"), Some(&Action::A));
        // the old Y binding of "y" was replaced, and "a" no longer maps to A
        assert!(!cfg.bindings.gamepad.contains_key("a"));
        assert_eq!(cfg.bindings.gamepad.get("x"), Some(&Action::B));
    }

    #[test]
    fn glyph_names_follow_the_controller_family() {
        assert_eq!(GlyphSet::Nintendo.source_label("a"), "B");
        assert_eq!(GlyphSet::PlayStation.source_label("a"), "Cross");
        assert_eq!(GlyphSet::Xbox.source_label("rightshoulder"), "RB");
    }

    /// A file written before profiles existed: no `[profiles]`, custom shared gamepad bindings, a glyph
    /// family and a dead zone.
    const LEGACY: &str = r#"
glyphs = "PlayStation"
deadzone = 0.3

[bindings.keyboard]
KeyZ = "A"

[bindings.gamepad]
a = "B"
x = "A"
"#;

    #[test]
    fn a_file_from_before_profiles_loads_and_seeds_the_profile_of_the_first_pad() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.toml");
        std::fs::write(&path, LEGACY).unwrap();
        let old = InputConfig::load_or_create(&path);
        assert!(old.profiles.is_empty());
        assert_eq!(old.glyphs, GlyphSet::PlayStation);
        assert_eq!(old.deadzone, 0.3);

        // the first pad of a model gets the old bindings and button names as its profile, and the file
        // gains the profile while everything else stays
        let mut c = Controls::new(path.clone());
        c.device_added(DeviceInfo {
            id: 1,
            guid: "03000000".into(),
            name: "Pad".into(),
        });
        let new = InputConfig::load_or_create(&path);
        let p = &new.profiles["03000000"];
        assert_eq!(p.name, "Pad");
        assert_eq!(p.glyphs, GlyphSet::PlayStation);
        assert_eq!(p.gamepad, old.bindings.gamepad);
        assert_eq!(new.bindings, old.bindings);
        assert_eq!((new.glyphs, new.deadzone), (old.glyphs, old.deadzone));
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("[profiles.03000000"));
    }

    #[test]
    fn a_model_gets_its_own_profile_and_finds_it_again() {
        let mut cfg = InputConfig::default();
        assert!(cfg.ensure_profile("xbox", "Xbox Controller"));
        assert!(cfg.ensure_profile("ps5", "DualSense"));
        cfg.set_glyphs_of(Some("ps5"), GlyphSet::PlayStation);
        cfg.bind(false, Some("ps5"), "b", Action::A);
        // known model: same profile, name refreshed, nothing reset
        assert!(!cfg.ensure_profile("ps5", "DualSense Wireless Controller"));
        assert_eq!(cfg.profiles["ps5"].name, "DualSense Wireless Controller");
        assert_eq!(cfg.glyphs_of(Some("ps5")), GlyphSet::PlayStation);
        assert_eq!(cfg.gamepad_map(Some("ps5")).get("b"), Some(&Action::A));
        // the other model and the shared bindings are untouched
        assert_eq!(cfg.glyphs_of(Some("xbox")), GlyphSet::GameCube);
        assert_eq!(cfg.gamepad_map(Some("xbox")), &Bindings::default().gamepad);
        assert_eq!(cfg.bindings, Bindings::default());
        // an unknown model falls back to the shared bindings
        assert_eq!(cfg.gamepad_map(Some("other")), &cfg.bindings.gamepad);
        // and it all survives the file
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.toml");
        cfg.save(&path).unwrap();
        assert_eq!(InputConfig::load_or_create(&path), cfg);
    }
}
