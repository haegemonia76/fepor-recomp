//! Everything the front end needs at run time: configuration, live state of every port, the pads plugged in
//! and the remapping dialog.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fepor_runtime::si::{self, PORTS};

use crate::{Action, Bindings, GlyphSet, InputConfig, InputState, RemapStep, Remapper};

/// How long a hot-plug notice stays on screen.
const NOTICE_FOR: Duration = Duration::from_secs(5);

/// A gamepad as the SDL layer reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// SDL's instance id: different for every plug-in, so it names the device while it stays plugged in.
    pub id: u32,
    /// SDL's GUID string: the same for every pad of one model, it selects the profile.
    pub guid: String,
    pub name: String,
}

/// A gamepad that is plugged in, and the GameCube port it drives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pad {
    pub info: DeviceInfo,
    /// 0-based GameCube port; `None` when all four are taken by other pads.
    pub port: Option<usize>,
}

/// Where an input event comes from.
#[derive(Clone, Copy)]
enum Origin {
    Keyboard,
    Pad(u32),
}

pub struct Controls {
    pub(crate) cfg: InputConfig,
    /// Live state of every GameCube port: the keyboard feeds port 1, each pad the port it is assigned.
    pub(crate) ports: [InputState; PORTS],
    pads: Vec<Pad>,
    /// The pad the player picked in the overlay to edit the profile of.
    selected: Option<u32>,
    remap: Option<Remapper>,
    path: PathBuf,
    /// A message for the player (shown in the window title) after a hotkey or remapping step.
    pub message: Option<String>,
    /// The latest hot-plug notice, shown over the game for a few seconds.
    notice: Option<(String, Instant)>,
    /// Sources of the device being remapped that are currently held down.
    held: HashSet<String>,
    suspended: bool,
    generation: u64,
}

impl Controls {
    pub fn new(path: PathBuf) -> Controls {
        Controls {
            cfg: InputConfig::load_or_create(&path),
            ports: Default::default(),
            pads: Vec::new(),
            selected: None,
            remap: None,
            path,
            message: None,
            notice: None,
            held: Default::default(),
            suspended: false,
            generation: 1,
        }
    }

    pub fn config(&self) -> &InputConfig {
        &self.cfg
    }

    /// Bumped whenever the bindings, the glyph family or the pad on port 1 change, so that whoever draws
    /// button icons knows when to redraw them.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn save(&self) {
        let _ = self.cfg.save(&self.path);
    }

    // ---- pads and ports ----

    /// The gamepads plugged in, in the order they were connected.
    pub fn pads(&self) -> &[Pad] {
        &self.pads
    }

    /// The latest hot-plug notice while it is fresh.
    pub fn notice(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTICE_FOR)
            .map(|(m, _)| m.as_str())
    }

    fn say(&mut self, text: String) {
        self.notice = Some((text.clone(), Instant::now()));
        self.message = Some(text);
    }

    fn pad_on(&self, port: usize) -> Option<&Pad> {
        self.pads.iter().find(|p| p.port == Some(port))
    }

    /// The profile key (GUID) of the pad on `port`.
    fn profile_on(&self, port: usize) -> Option<&str> {
        self.pad_on(port).map(|p| p.info.guid.as_str())
    }

    fn free_port(&self) -> Option<usize> {
        (0..PORTS).find(|p| self.pad_on(*p).is_none())
    }

    fn publish_port(&self, port: usize) {
        si::set_pad(port, self.ports[port].pad(&self.cfg, self.profile_on(port)));
    }

    fn publish_all(&self) {
        for port in 0..PORTS {
            self.publish_port(port);
            si::set_port_connected(port, port == 0 || self.pad_on(port).is_some());
        }
    }

    /// A gamepad was plugged in (at start, or while the game runs): it gets the profile of its model
    /// (created from the shared bindings the first time) and the lowest free port.
    pub fn device_added(&mut self, info: DeviceInfo) {
        if self.pads.iter().any(|p| p.info.id == info.id) {
            return;
        }
        let created = self.cfg.ensure_profile(&info.guid, &info.name);
        self.save();
        let port = self.free_port();
        let text = match port {
            Some(p) => format!(
                "Gamepad connected: {} (player {}{})",
                info.name,
                p + 1,
                if created { ", new profile" } else { "" }
            ),
            None => format!(
                "Gamepad connected: {} (all four ports are taken)",
                info.name
            ),
        };
        self.pads.push(Pad { info, port });
        self.generation += 1;
        self.publish_all();
        self.say(text);
    }

    /// A gamepad was unplugged: its port reads as no controller, and whatever it held is let go.
    pub fn device_removed(&mut self, id: u32) {
        let Some(i) = self.pads.iter().position(|p| p.info.id == id) else {
            return;
        };
        let pad = self.pads.remove(i);
        if let Some(port) = pad.port {
            self.ports[port].release_all(false);
        }
        if self.selected == Some(id) {
            self.selected = None;
        }
        // a pad that had no port takes the one just freed
        for i in 0..self.pads.len() {
            if self.pads[i].port.is_none() {
                self.pads[i].port = self.free_port();
            }
        }
        self.generation += 1;
        self.publish_all();
        self.say(format!("Gamepad disconnected: {}", pad.info.name));
    }

    /// Move the pad `id` to `port`; a pad already there takes the port the first one leaves.
    pub fn set_port(&mut self, id: u32, port: usize) {
        let Some(i) = self.pads.iter().position(|p| p.info.id == id) else {
            return;
        };
        if port >= PORTS || self.pads[i].port == Some(port) {
            return;
        }
        let old = self.pads[i].port;
        if let Some(other) = self.pads.iter_mut().find(|p| p.port == Some(port)) {
            other.port = old;
        }
        self.pads[i].port = Some(port);
        for p in 0..PORTS {
            self.ports[p].release_all(false);
        }
        self.generation += 1;
        self.publish_all();
    }

    /// Pick the pad whose profile the overlay edits.
    pub fn select_pad(&mut self, id: u32) {
        if self.pads.iter().any(|p| p.info.id == id) {
            self.selected = Some(id);
        }
    }

    /// The pad whose profile is being edited: the one picked, else the one on port 1, else the first.
    pub fn edited_pad(&self) -> Option<&Pad> {
        self.selected
            .and_then(|id| self.pads.iter().find(|p| p.info.id == id))
            .or_else(|| self.pad_on(0))
            .or_else(|| self.pads.first())
    }

    fn edit_key(&self) -> Option<&str> {
        self.edited_pad().map(|p| p.info.guid.as_str())
    }

    /// The button-name family of the profile being edited (the shared one while no pad is plugged in).
    pub fn glyphs(&self) -> GlyphSet {
        self.cfg.glyphs_of(self.edit_key())
    }

    /// The gamepad bindings of the profile being edited.
    pub fn gamepad_bindings(&self) -> &std::collections::BTreeMap<String, Action> {
        self.cfg.gamepad_map(self.edit_key())
    }

    /// Gamepad sources of the profile being edited that do `action`.
    pub fn gamepad_sources_of(&self, action: Action) -> Vec<&str> {
        self.cfg.sources_of(false, self.edit_key(), action)
    }

    /// The button-name family and gamepad input the player of port 1 uses for `action`, for the icons the
    /// game draws.
    pub fn icon_source(&self, action: Action) -> Option<(GlyphSet, String)> {
        let profile = self.profile_on(0);
        let mut sources = self.cfg.sources_of(false, profile, action);
        sources.sort_unstable();
        Some((self.cfg.glyphs_of(profile), sources.first()?.to_string()))
    }

    // ---- remapping and editing ----

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
        let profile = self.edit_key().map(String::from);
        self.begin_remap(Remapper::new(keyboard, profile));
    }

    fn release_all(&mut self, keyboard: bool) {
        for state in &mut self.ports {
            state.release_all(keyboard);
        }
    }

    fn begin_remap(&mut self, remapper: Remapper) {
        let keyboard = remapper.keyboard();
        // inputs already held when remapping starts must be released before they can count
        let prefix = if keyboard { "kb:" } else { "pad:" };
        self.held = self
            .ports
            .iter()
            .flat_map(|s| s.sources.keys())
            .filter_map(|k| k.strip_prefix(prefix).map(String::from))
            .collect();
        self.release_all(true);
        self.release_all(false);
        self.publish_all();
        self.remap = Some(remapper);
        self.message = self.prompt();
    }

    /// Switch to the next button-glyph family and save the choice.
    pub fn cycle_glyphs(&mut self) {
        let next = match self.glyphs() {
            GlyphSet::GameCube => GlyphSet::Xbox,
            GlyphSet::Xbox => GlyphSet::Nintendo,
            GlyphSet::Nintendo => GlyphSet::PlayStation,
            GlyphSet::PlayStation => GlyphSet::GameCube,
        };
        self.set_glyphs(next);
        self.message = Some(format!("Button glyphs: {next:?}"));
    }

    /// Rebind one action of the keyboard or of the gamepad profile being edited: the next input becomes its
    /// binding (Esc on the keyboard keeps the current one). Saved when done.
    pub fn rebind(&mut self, keyboard: bool, action: Action) {
        let profile = self.edit_key().map(String::from);
        self.begin_remap(Remapper::single(keyboard, profile, action));
    }

    /// Whether the next input is being captured as a binding.
    pub fn is_remapping(&self) -> bool {
        self.remap.is_some()
    }

    /// The device (`true` = keyboard) and action being captured, if any.
    pub fn remap_target(&self) -> Option<(bool, Action)> {
        let r = self.remap.as_ref()?;
        match r.current() {
            RemapStep::Ask(a) => Some((r.keyboard(), a)),
            RemapStep::Done => None,
        }
    }

    pub fn cancel_remap(&mut self) {
        if self.remap.take().is_some() {
            self.message = Some("Remapping cancelled".into());
        }
    }

    /// While suspended (the overlay is open) inputs no longer reach the game: every pad reads neutral.
    /// Remapping still receives them.
    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
        if suspended {
            self.release_all(true);
            self.release_all(false);
            self.publish_all();
        }
    }

    /// Set the button-name family of the profile being edited and save it.
    pub fn set_glyphs(&mut self, glyphs: GlyphSet) {
        let key = self.edit_key().map(String::from);
        self.cfg.set_glyphs_of(key.as_deref(), glyphs);
        self.generation += 1;
        self.save();
    }

    pub fn set_deadzone(&mut self, deadzone: f32) {
        self.cfg.deadzone = deadzone.clamp(0.0, 0.9);
        self.save();
    }

    /// Back to the default bindings: the keyboard's and those of the profile being edited (glyph family and
    /// dead zone kept).
    pub fn reset_bindings(&mut self) {
        let key = self.edit_key().map(String::from);
        let defaults = Bindings::default();
        self.cfg.bindings.keyboard = defaults.keyboard;
        match key.as_deref().and_then(|g| self.cfg.profiles.get_mut(g)) {
            Some(p) => p.gamepad = defaults.gamepad,
            None => self.cfg.bindings.gamepad = defaults.gamepad,
        }
        self.generation += 1;
        self.save();
    }

    /// Bind the input `source` of the keyboard or of the gamepad profile being edited to `action` (the
    /// action's earlier source on that device is released, as is the action the source had). Saved at once.
    pub fn assign(&mut self, keyboard: bool, source: &str, action: Action) {
        let key = self.edit_key().map(String::from);
        self.cfg.bind(keyboard, key.as_deref(), source, action);
        self.generation += 1;
        self.save();
    }

    /// Leave `source` unbound.
    pub fn unassign(&mut self, keyboard: bool, source: &str) {
        let key = self.edit_key().map(String::from);
        if self.cfg.unbind(keyboard, key.as_deref(), source) {
            self.generation += 1;
            self.save();
        }
    }

    // ---- input events ----

    /// A key went down (`value` 1) or up (0); `source` is winit's `KeyCode` name.
    pub fn keyboard_input(&mut self, source: &str, value: f32) {
        self.input(Origin::Keyboard, source, value);
    }

    /// An input of the pad `id` changed; `source` is SDL's name for it and `value` its strength.
    pub fn pad_input(&mut self, id: u32, source: &str, value: f32) {
        self.input(Origin::Pad(id), source, value);
    }

    fn input(&mut self, from: Origin, source: &str, value: f32) {
        let (keyboard, port) = match from {
            Origin::Keyboard => (true, Some(0)),
            Origin::Pad(id) => match self.pads.iter().find(|p| p.info.id == id) {
                Some(p) => (false, p.port),
                None => return,
            },
        };
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
                    self.generation += 1;
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
        if self.suspended {
            return;
        }
        let Some(port) = port else { return };
        self.ports[port].set(keyboard, source, value);
        self.publish_port(port);
    }

    /// Release every key (the window lost focus).
    pub fn release_keyboard(&mut self) {
        self.ports[0].release_all(true);
        self.publish_port(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fepor_runtime::si::button;

    fn info(id: u32, guid: &str) -> DeviceInfo {
        DeviceInfo {
            id,
            guid: guid.into(),
            name: format!("Pad {guid}"),
        }
    }

    fn controls() -> (Controls, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let c = Controls::new(dir.path().join("input.toml"));
        (c, dir)
    }

    /// What the game sees on `port` right now.
    fn game_pad(c: &Controls, port: usize) -> fepor_runtime::si::PadState {
        c.ports[port].pad(&c.cfg, c.profile_on(port))
    }

    #[test]
    fn interactive_remap_saves_and_the_new_binding_drives_the_pad() {
        let (mut c, dir) = controls();
        c.device_added(info(7, "model-a"));
        c.toggle_remap(false);
        assert!(c
            .message
            .as_deref()
            .unwrap()
            .contains("press the input for A"));
        // action A takes the "y" button; a press needs its release before the next action is asked
        c.pad_input(7, "y", 1.0);
        c.pad_input(7, "y", 0.0);
        // inputs from the other device are ignored while remapping
        c.keyboard_input("KeyZ", 1.0);
        for n in 1..Action::ALL.len() {
            let source = format!("extra{n}");
            c.pad_input(7, &source, 1.0);
            c.pad_input(7, &source, 0.0);
        }
        assert_eq!(c.message.as_deref(), Some("Controls saved"));
        let saved = InputConfig::load_or_create(&dir.path().join("input.toml"));
        // the binding went to the profile of the pad, not to the shared bindings
        assert_eq!(saved.profiles["model-a"].gamepad.get("y"), Some(&Action::A));
        assert_eq!(saved.bindings.gamepad.get("a"), Some(&Action::A));
        assert_eq!(saved.bindings.keyboard.get("KeyZ"), Some(&Action::A));
        // after remapping the new binding presses A
        c.pad_input(7, "y", 1.0);
        assert_eq!(game_pad(&c, 0).buttons & button::A, button::A);
    }

    #[test]
    fn a_held_analog_trigger_binds_one_action_until_it_is_released() {
        let (mut c, _dir) = controls();
        c.device_added(info(1, "model-a"));
        c.toggle_remap(false);
        // the trigger keeps reporting while held: A takes it, B and the rest must not
        for v in [0.7, 0.9, 1.0, 1.0, 0.95] {
            c.pad_input(1, "lefttrigger", v);
        }
        assert_eq!(c.gamepad_bindings().get("lefttrigger"), Some(&Action::A));
        assert!(
            c.message
                .as_deref()
                .unwrap()
                .contains("press the input for B"),
            "{:?}",
            c.message
        );
        // released, the next press goes to B
        c.pad_input(1, "lefttrigger", 0.0);
        c.pad_input(1, "righttrigger", 1.0);
        assert_eq!(c.gamepad_bindings().get("righttrigger"), Some(&Action::B));
        // axis jitter between 0.3 and 0.6 neither releases nor presses
        c.pad_input(1, "righttrigger", 0.5);
        c.pad_input(1, "righttrigger", 0.8);
        assert!(c
            .message
            .as_deref()
            .unwrap()
            .contains("press the input for X"));
    }

    #[test]
    fn pads_take_the_lowest_free_ports_and_hot_plug_frees_them() {
        let (mut c, _dir) = controls();
        for (id, guid) in [(1, "a"), (2, "b"), (3, "a"), (4, "c")] {
            c.device_added(info(id, guid));
        }
        let ports = |c: &Controls| c.pads().iter().map(|p| p.port).collect::<Vec<_>>();
        assert_eq!(ports(&c), [Some(0), Some(1), Some(2), Some(3)]);
        // a fifth pad has no port but is still known
        c.device_added(info(5, "d"));
        assert_eq!(c.pads()[4].port, None);
        assert!(c.notice().unwrap().contains("all four ports are taken"));
        c.pad_input(5, "a", 1.0);
        for port in 0..PORTS {
            assert_eq!(game_pad(&c, port).buttons, 0);
        }
        // unplug the pad on port 2: the pad that had no port takes it
        c.device_removed(2);
        assert_eq!(c.notice(), Some("Gamepad disconnected: Pad b"));
        assert_eq!(ports(&c), [Some(0), Some(2), Some(3), Some(1)]);
        c.device_added(info(6, "b"));
        assert_eq!(c.pads().last().unwrap().port, None);
        // an unplugged id is forgotten: late events from it do nothing
        c.device_removed(6);
        c.pad_input(6, "a", 1.0);
        assert_eq!(game_pad(&c, 1).buttons, 0);
    }

    #[test]
    fn each_pad_drives_only_its_port_through_its_own_profile() {
        let (mut c, _dir) = controls();
        c.device_added(info(1, "a"));
        c.device_added(info(2, "b"));
        // model b has "a" doing GameCube B
        c.select_pad(2);
        c.assign(false, "a", Action::B);
        c.pad_input(1, "a", 1.0);
        c.pad_input(2, "a", 1.0);
        assert_eq!(game_pad(&c, 0).buttons, button::A);
        assert_eq!(game_pad(&c, 1).buttons, button::B);
        // the keyboard is player 1's, whatever pads are plugged in
        c.keyboard_input("KeyX", 1.0);
        assert_eq!(game_pad(&c, 0).buttons, button::A | button::B);
        assert_eq!(game_pad(&c, 1).buttons, button::B);
        // unplugging pad 1 lets go of what it held
        c.device_removed(1);
        assert_eq!(game_pad(&c, 0).buttons, button::B);
    }

    #[test]
    fn set_port_swaps_pads_and_a_pad_keeps_its_profile() {
        let (mut c, _dir) = controls();
        c.device_added(info(1, "a"));
        c.device_added(info(2, "b"));
        c.set_port(2, 0);
        assert_eq!(c.pads()[0].port, Some(1));
        assert_eq!(c.pads()[1].port, Some(0));
        c.set_port(2, 3);
        assert_eq!(c.pads()[1].port, Some(3));
        assert_eq!(c.pad_on(0), None);
        // port 1 has no pad any more: icons fall back to the shared bindings
        assert!(c.icon_source(Action::A).is_some());
    }

    #[test]
    fn the_overlay_edits_the_profile_of_the_selected_pad_only() {
        let (mut c, dir) = controls();
        c.device_added(info(1, "a"));
        c.device_added(info(2, "b"));
        // without a pick, the pad on port 1
        c.set_glyphs(GlyphSet::PlayStation);
        c.select_pad(2);
        assert_eq!(c.glyphs(), GlyphSet::GameCube);
        c.set_glyphs(GlyphSet::Xbox);
        c.reset_bindings();
        let saved = InputConfig::load_or_create(&dir.path().join("input.toml"));
        assert_eq!(saved.profiles["a"].glyphs, GlyphSet::PlayStation);
        assert_eq!(saved.profiles["b"].glyphs, GlyphSet::Xbox);
        // the icons the game draws follow player 1's pad
        assert_eq!(c.icon_source(Action::A).unwrap().0, GlyphSet::PlayStation);
    }

    #[test]
    fn nothing_reaches_the_game_while_the_overlay_has_the_pads() {
        let (mut c, _dir) = controls();
        c.device_added(info(1, "a"));
        c.pad_input(1, "a", 1.0);
        assert_eq!(game_pad(&c, 0).buttons, button::A);
        c.set_suspended(true);
        assert_eq!(game_pad(&c, 0).buttons, 0);
        c.pad_input(1, "b", 1.0);
        assert_eq!(game_pad(&c, 0).buttons, 0);
    }
}
