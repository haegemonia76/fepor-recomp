//! Everything the front end needs at run time: configuration, live state of the virtual pad, the gamepads
//! plugged in and the remapping dialog.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fepor_runtime::si;

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

impl DeviceInfo {
    /// The name to show: the pad's own, or a generic one when it reports none.
    pub fn display_name(&self) -> &str {
        match self.name.trim() {
            "" => "Unknown gamepad",
            n => n,
        }
    }
}

/// Where an input event comes from.
#[derive(Clone, Copy)]
enum Origin {
    Keyboard,
    Pad(u32),
}

/// Strength at which an input counts as a press (it makes its pad the active one).
const PRESS: f32 = 0.6;

pub struct Controls {
    pub(crate) cfg: InputConfig,
    /// Live state of the virtual pad: the keyboard and the active gamepad feed it.
    pub(crate) state: InputState,
    pads: Vec<DeviceInfo>,
    /// GUIDs of the models plugged in so far, in order of first plug (numbers the profiles of pads that
    /// report no name).
    models: Vec<String>,
    /// The gamepad that drives the game: the one plugged in last or pressed last (the first one when that
    /// one is gone).
    active: Option<u32>,
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
            state: InputState::default(),
            pads: Vec::new(),
            models: Vec::new(),
            active: None,
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

    /// Bumped whenever the bindings, the glyph family or the active pad changes, so that whoever draws
    /// button icons knows when to redraw them.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn save(&self) {
        let _ = self.cfg.save(&self.path);
    }

    // ---- pads ----

    /// The gamepads plugged in, in the order they were connected.
    pub fn pads(&self) -> &[DeviceInfo] {
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

    /// How to call the profile of pad `id` next to the pad's own name, and the hover text with the details:
    /// a short wording when the pad's name already says which model it is, `Profile 1`, `Profile 2`... in
    /// order of first plug when the pad reports no name.
    pub fn profile_label(&self, id: u32) -> Option<(String, String)> {
        let pad = self.pads.iter().find(|p| p.id == id)?;
        let sharing = self.pads.iter().filter(|p| p.guid == pad.guid).count();
        let number = self.models.iter().position(|g| *g == pad.guid)? + 1;
        let (text, name) = if pad.name.trim().is_empty() {
            let n = format!("Profile {number}");
            (n.clone(), n)
        } else if sharing > 1 {
            (
                format!("Profile shared by {sharing} pads"),
                format!("Profile: {}", pad.name),
            )
        } else {
            (
                "Profile of this model".to_string(),
                format!("Profile: {}", pad.name),
            )
        };
        Some((text, format!("{name}\nGUID {}", pad.guid)))
    }

    /// The gamepad that drives the game.
    pub fn active_pad(&self) -> Option<&DeviceInfo> {
        self.active
            .and_then(|id| self.pads.iter().find(|p| p.id == id))
            .or_else(|| self.pads.first())
    }

    fn active_key(&self) -> Option<&str> {
        self.active_pad().map(|p| p.guid.as_str())
    }

    fn publish(&self) {
        si::set_pad(self.state.pad(&self.cfg, self.active_key()));
    }

    /// Make `id` the pad that drives the game; what the previous one held is let go.
    fn activate(&mut self, id: u32) {
        if self.active == Some(id) {
            return;
        }
        self.active = Some(id);
        self.state.release_all(false);
        self.generation += 1;
        self.publish();
    }

    /// A gamepad was plugged in (at start, or while the game runs): it gets the profile of its model
    /// (created from the shared bindings the first time) and drives the game from now on.
    pub fn device_added(&mut self, info: DeviceInfo) {
        if self.pads.iter().any(|p| p.id == info.id) {
            return;
        }
        if !self.models.contains(&info.guid) {
            self.models.push(info.guid.clone());
        }
        let created = self.cfg.ensure_profile(&info.guid, &info.name);
        self.save();
        let text = format!(
            "Gamepad connected: {}{}",
            info.display_name(),
            if created { " (new profile)" } else { "" }
        );
        let id = info.id;
        self.pads.push(info);
        self.activate(id);
        self.generation += 1;
        self.say(text);
    }

    /// A gamepad was unplugged: whatever it held is let go.
    pub fn device_removed(&mut self, id: u32) {
        let Some(i) = self.pads.iter().position(|p| p.id == id) else {
            return;
        };
        let pad = self.pads.remove(i);
        if self.active == Some(id) {
            self.active = None;
            self.state.release_all(false);
        }
        if self.selected == Some(id) {
            self.selected = None;
        }
        self.generation += 1;
        self.publish();
        self.say(format!("Gamepad disconnected: {}", pad.display_name()));
    }

    /// Pick the pad whose profile the overlay edits.
    pub fn select_pad(&mut self, id: u32) {
        if self.pads.iter().any(|p| p.id == id) {
            self.selected = Some(id);
        }
    }

    /// The pad whose profile is being edited: the one picked, else the active one.
    pub fn edited_pad(&self) -> Option<&DeviceInfo> {
        self.selected
            .and_then(|id| self.pads.iter().find(|p| p.id == id))
            .or_else(|| self.active_pad())
    }

    fn edit_key(&self) -> Option<&str> {
        self.edited_pad().map(|p| p.guid.as_str())
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

    /// The button-name family and gamepad input the active pad uses for `action`, for the icons the
    /// game draws.
    pub fn icon_source(&self, action: Action) -> Option<(GlyphSet, String)> {
        let profile = self.active_key();
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

    fn begin_remap(&mut self, remapper: Remapper) {
        let keyboard = remapper.keyboard();
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

    /// While suspended (the overlay is open) inputs no longer reach the game: the pad reads neutral.
    /// Remapping still receives them.
    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
        if suspended {
            self.state.release_all(true);
            self.state.release_all(false);
            self.publish();
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
        let keyboard = match from {
            Origin::Keyboard => true,
            Origin::Pad(id) => {
                if !self.pads.iter().any(|p| p.id == id) {
                    return;
                }
                // pressing a pad makes it the active one; the others are ignored until they are pressed
                if self.remap.is_none() && !self.suspended && value >= PRESS {
                    self.activate(id);
                }
                if self.remap.is_none() && self.active_pad().map(|p| p.id) != Some(id) {
                    return;
                }
                false
            }
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
        self.state.set(keyboard, source, value);
        self.publish();
    }

    /// Release every key (the window lost focus).
    pub fn release_keyboard(&mut self) {
        self.state.release_all(true);
        self.publish();
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

    /// What the game sees right now.
    fn game_pad(c: &Controls) -> fepor_runtime::si::PadState {
        c.state.pad(&c.cfg, c.active_key())
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
        assert_eq!(game_pad(&c).buttons & button::A, button::A);
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
    fn a_pad_plugged_in_drives_the_game_and_unplugging_lets_go() {
        let (mut c, _dir) = controls();
        assert!(c.active_pad().is_none());
        c.device_added(info(1, "a"));
        assert_eq!(c.notice(), Some("Gamepad connected: Pad a (new profile)"));
        c.pad_input(1, "a", 1.0);
        assert_eq!(game_pad(&c).buttons, button::A);
        c.device_removed(1);
        assert_eq!(c.notice(), Some("Gamepad disconnected: Pad a"));
        assert_eq!(game_pad(&c).buttons, 0, "a held button is let go");
        // late events of an unplugged pad do nothing; plugging it in again (new id, same model) finds the
        // profile it already has
        c.pad_input(1, "a", 1.0);
        assert_eq!(game_pad(&c).buttons, 0);
        c.device_added(info(2, "a"));
        assert_eq!(c.notice(), Some("Gamepad connected: Pad a"));
        c.pad_input(2, "a", 1.0);
        assert_eq!(game_pad(&c).buttons, button::A);
    }

    #[test]
    fn with_several_pads_the_one_plugged_in_or_pressed_last_drives_the_game() {
        let (mut c, _dir) = controls();
        c.device_added(info(1, "a"));
        c.device_added(info(2, "b"));
        assert_eq!(c.active_pad().unwrap().id, 2, "the newcomer is active");
        // pad 1 is ignored until it is pressed; model b has "a" doing GameCube B
        c.select_pad(2);
        c.assign(false, "a", Action::B);
        c.pad_input(2, "a", 1.0);
        assert_eq!(game_pad(&c).buttons, button::B);
        c.pad_input(2, "a", 0.0);
        c.pad_input(1, "a", 0.3);
        assert_eq!(c.active_pad().unwrap().id, 2);
        // pressing pad 1 takes over, with its own profile, and what pad 2 held is let go
        c.pad_input(2, "a", 1.0);
        c.pad_input(1, "a", 1.0);
        assert_eq!(c.active_pad().unwrap().id, 1);
        assert_eq!(game_pad(&c).buttons, button::A);
        c.pad_input(2, "a", 0.0);
        assert_eq!(game_pad(&c).buttons, button::A);
        // the keyboard always works
        c.keyboard_input("KeyX", 1.0);
        assert_eq!(game_pad(&c).buttons, button::A | button::B);
        // unplugging the active pad hands over to the one left
        c.device_removed(1);
        assert_eq!(c.active_pad().unwrap().id, 2);
        assert_eq!(game_pad(&c).buttons, button::B);
    }

    #[test]
    fn the_overlay_edits_the_profile_of_the_selected_pad_only() {
        let (mut c, dir) = controls();
        c.device_added(info(1, "a"));
        c.device_added(info(2, "b"));
        // without a pick, the active pad (the last plugged in)
        c.set_glyphs(GlyphSet::PlayStation);
        c.select_pad(1);
        assert_eq!(c.glyphs(), GlyphSet::GameCube);
        c.set_glyphs(GlyphSet::Xbox);
        c.reset_bindings();
        let saved = InputConfig::load_or_create(&dir.path().join("input.toml"));
        assert_eq!(saved.profiles["b"].glyphs, GlyphSet::PlayStation);
        assert_eq!(saved.profiles["a"].glyphs, GlyphSet::Xbox);
        // the icons the game draws follow the active pad
        assert_eq!(c.icon_source(Action::A).unwrap().0, GlyphSet::PlayStation);
    }

    #[test]
    fn profile_labels_are_short_and_numbered_for_unnamed_pads() {
        let (mut c, _dir) = controls();
        c.device_added(info(1, "a"));
        assert_eq!(c.profile_label(1).unwrap().0, "Profile of this model");
        assert_eq!(c.profile_label(1).unwrap().1, "Profile: Pad a\nGUID a");
        c.device_added(info(2, "a"));
        assert_eq!(c.profile_label(2).unwrap().0, "Profile shared by 2 pads");
        // no name: the pad is called generically and its profile numbered in order of first plug
        let unnamed = |id, guid: &str| DeviceInfo {
            id,
            guid: guid.into(),
            name: "  ".into(),
        };
        c.device_added(unnamed(3, "x"));
        c.device_added(unnamed(4, "y"));
        assert_eq!(c.pads()[2].display_name(), "Unknown gamepad");
        assert_eq!(
            c.notice(),
            Some("Gamepad connected: Unknown gamepad (new profile)")
        );
        assert_eq!(c.profile_label(3).unwrap().0, "Profile 2");
        assert_eq!(c.profile_label(4).unwrap().0, "Profile 3");
        assert!(c.profile_label(99).is_none());
    }

    #[test]
    fn nothing_reaches_the_game_while_the_overlay_has_the_pads() {
        let (mut c, _dir) = controls();
        c.device_added(info(1, "a"));
        c.pad_input(1, "a", 1.0);
        assert_eq!(game_pad(&c).buttons, button::A);
        c.set_suspended(true);
        assert_eq!(game_pad(&c).buttons, 0);
        c.pad_input(1, "b", 1.0);
        assert_eq!(game_pad(&c).buttons, 0);
    }
}
