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

pub mod gamepad;

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
}

/// Which family of button icons to show for GameCube buttons the player has mapped.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub glyphs: GlyphSet,
    /// Analog values below this (0..1) read as zero.
    pub deadzone: f32,
    pub bindings: Bindings,
}

impl Default for InputConfig {
    fn default() -> Self {
        InputConfig {
            glyphs: GlyphSet::default(),
            deadzone: 0.15,
            bindings: Bindings::default(),
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
                let c = InputConfig::default();
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

    /// Bind `source` (a keyboard key when `keyboard`, otherwise a gamepad input) to `action`, replacing any
    /// earlier binding of that source and any other source the same device had for the action.
    pub fn bind(&mut self, keyboard: bool, source: &str, action: Action) {
        let map = if keyboard {
            &mut self.bindings.keyboard
        } else {
            &mut self.bindings.gamepad
        };
        map.retain(|_, a| *a != action);
        map.insert(source.to_string(), action);
    }

    /// Sources bound to `action` on the keyboard or the gamepad.
    pub fn sources_of(&self, keyboard: bool, action: Action) -> Vec<&str> {
        let map = if keyboard {
            &self.bindings.keyboard
        } else {
            &self.bindings.gamepad
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
    sources: HashMap<String, f32>,
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

    fn strength(&self, cfg: &InputConfig, action: Action) -> f32 {
        let mut best: f32 = 0.0;
        for (keyboard, map) in [
            (true, &cfg.bindings.keyboard),
            (false, &cfg.bindings.gamepad),
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

    /// The controller state the GameCube sees.
    pub fn pad(&self, cfg: &InputConfig) -> PadState {
        let s = |a| self.strength(cfg, a);
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
    pub fn new(keyboard: bool) -> Remapper {
        Remapper {
            keyboard,
            queue: Action::ALL.to_vec(),
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
            cfg.bind(self.keyboard, src, a);
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
        c.bind(true, "KeyF", Action::A);
        c.glyphs = GlyphSet::PlayStation;
        c.save(&path).unwrap();
        let back = InputConfig::load_or_create(&path);
        assert_eq!(back, c);
        assert_eq!(back.sources_of(true, Action::A), vec!["KeyF"]);
    }

    #[test]
    fn digital_and_analog_sources_make_a_pad() {
        let cfg = InputConfig::default();
        let mut st = InputState::default();
        st.set(true, "KeyZ", 1.0);
        st.set(true, "ArrowRight", 1.0);
        st.set(false, "lefttrigger", 0.6);
        let p = st.pad(&cfg);
        assert_eq!(p.buttons & button::A, button::A);
        assert_eq!(p.stick.0, 228);
        assert_eq!(p.stick.1, 128);
        // a half-pressed trigger moves the analog value without clicking
        assert_eq!(p.buttons & button::L, 0);
        assert!(p.triggers.0 > 100 && p.triggers.0 < 160, "{}", p.triggers.0);
        st.set(false, "lefttrigger", 1.0);
        assert_eq!(st.pad(&cfg).buttons & button::L, button::L);
    }

    #[test]
    fn deadzone_hides_small_axis_values() {
        let cfg = InputConfig::default();
        let mut st = InputState::default();
        st.set(false, "leftx+", 0.1);
        assert_eq!(st.pad(&cfg).stick, (128, 128));
    }

    #[test]
    fn remapping_replaces_bindings_and_a_skip_keeps_them() {
        let mut cfg = InputConfig::default();
        let mut r = Remapper::new(false);
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

    #[test]
    fn interactive_remap_saves_and_the_new_binding_drives_the_pad() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.toml");
        let mut c = Controls::new(path.clone());
        c.toggle_remap(false);
        assert!(c
            .message
            .as_deref()
            .unwrap()
            .contains("press the input for A"));
        // action A takes the "y" button; a press needs its release before the next action is asked
        c.input(false, "y", 1.0);
        c.input(false, "y", 0.0);
        // inputs from the other device are ignored while remapping
        c.input(true, "KeyZ", 1.0);
        for n in 1..Action::ALL.len() {
            let source = format!("extra{n}");
            c.input(false, &source, 1.0);
            c.input(false, &source, 0.0);
        }
        assert_eq!(c.message.as_deref(), Some("Controls saved"));
        let saved = InputConfig::load_or_create(&path);
        assert_eq!(saved.bindings.gamepad.get("y"), Some(&Action::A));
        assert_eq!(saved.bindings.keyboard.get("KeyZ"), Some(&Action::A));
        // after remapping the new binding presses A
        c.input(false, "y", 1.0);
        assert_eq!(c.state.pad(&c.cfg).buttons & button::A, button::A);
    }

    #[test]
    fn a_held_analog_trigger_binds_one_action_until_it_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Controls::new(dir.path().join("input.toml"));
        c.toggle_remap(false);
        // the trigger keeps reporting while held: A takes it, B and the rest must not
        for v in [0.7, 0.9, 1.0, 1.0, 0.95] {
            c.input(false, "lefttrigger", v);
        }
        assert_eq!(
            c.config().bindings.gamepad.get("lefttrigger"),
            Some(&Action::A)
        );
        assert!(
            c.message
                .as_deref()
                .unwrap()
                .contains("press the input for B"),
            "{:?}",
            c.message
        );
        // released, the next press goes to B
        c.input(false, "lefttrigger", 0.0);
        c.input(false, "righttrigger", 1.0);
        assert_eq!(
            c.config().bindings.gamepad.get("righttrigger"),
            Some(&Action::B)
        );
        // axis jitter between 0.3 and 0.6 neither releases nor presses
        c.input(false, "righttrigger", 0.5);
        c.input(false, "righttrigger", 0.8);
        assert!(c
            .message
            .as_deref()
            .unwrap()
            .contains("press the input for X"));
    }
}

/// Everything the front end needs at run time: configuration, live state and the remapping dialog.
pub struct Controls {
    cfg: InputConfig,
    state: InputState,
    remap: Option<Remapper>,
    path: std::path::PathBuf,
    /// A message for the player (shown in the window title) after a hotkey or remapping step.
    pub message: Option<String>,
    /// Sources of the device being remapped that are currently held down.
    held: std::collections::HashSet<String>,
}

impl Controls {
    pub fn new(path: std::path::PathBuf) -> Controls {
        Controls {
            cfg: InputConfig::load_or_create(&path),
            state: InputState::default(),
            remap: None,
            path,
            message: None,
            held: Default::default(),
        }
    }

    pub fn config(&self) -> &InputConfig {
        &self.cfg
    }

    fn publish(&self) {
        fepor_runtime::si::set_pad(self.state.pad(&self.cfg));
    }

    fn prompt(&self) -> Option<String> {
        let r = self.remap.as_ref()?;
        match r.current() {
            RemapStep::Ask(a) => Some(format!(
                "Remapping {}: press the input for {} (Esc: keep, F2/F3: cancel)",
                if r.keyboard() { "keyboard" } else { "gamepad" },
                a.label()
            )),
            RemapStep::Done => None,
        }
    }

    /// Start (or cancel) interactive remapping of the keyboard or the gamepad.
    pub fn toggle_remap(&mut self, keyboard: bool) {
        if self
            .remap
            .as_ref()
            .is_some_and(|r| r.keyboard() == keyboard)
        {
            self.remap = None;
            self.message = Some("Remapping cancelled".into());
            return;
        }
        // inputs already held when remapping starts must be released before they can count
        let prefix = if keyboard { "kb:" } else { "pad:" };
        self.held = self
            .state
            .sources
            .keys()
            .filter_map(|k| k.strip_prefix(prefix).map(String::from))
            .collect();
        self.state.release_all(true);
        self.state.release_all(false);
        self.publish();
        self.remap = Some(Remapper::new(keyboard));
        self.message = self.prompt();
    }

    /// Switch to the next button-glyph family and save the choice.
    pub fn cycle_glyphs(&mut self) {
        self.cfg.glyphs = match self.cfg.glyphs {
            GlyphSet::GameCube => GlyphSet::Xbox,
            GlyphSet::Xbox => GlyphSet::Nintendo,
            GlyphSet::Nintendo => GlyphSet::PlayStation,
            GlyphSet::PlayStation => GlyphSet::GameCube,
        };
        let _ = self.cfg.save(&self.path);
        self.message = Some(format!("Button glyphs: {:?}", self.cfg.glyphs));
    }

    /// One input event. `keyboard` selects the device; `value` is 0 on release.
    pub fn input(&mut self, keyboard: bool, source: &str, value: f32) {
        if let Some(r) = &mut self.remap {
            // Only the device being remapped counts, and only on the press edge: an analog trigger keeps
            // reporting values while it is held, and must not bind every following action to itself.
            // A source is "held" from 0.6 until it falls below 0.3 (hysteresis against axis jitter).
            if r.keyboard() == keyboard {
                if value < 0.3 {
                    self.held.remove(source);
                } else if value >= 0.6 && self.held.insert(source.to_string()) {
                    let step = if keyboard && source == "Escape" {
                        r.feed(&mut self.cfg, None)
                    } else {
                        r.feed(&mut self.cfg, Some(source))
                    };
                    match step {
                        RemapStep::Done => {
                            let _ = self.cfg.save(&self.path);
                            self.remap = None;
                            self.message = Some("Controls saved".into());
                        }
                        RemapStep::Ask(_) => self.message = self.prompt(),
                    }
                }
            }
            return;
        }
        self.state.set(keyboard, source, value);
        self.publish();
    }

    /// Release every key and button (the window lost focus).
    pub fn release_keyboard(&mut self) {
        self.state.release_all(true);
        self.publish();
    }
}
