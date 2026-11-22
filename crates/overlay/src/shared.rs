//! State shared between the threads: the window thread feeds events in, the render thread draws the UI.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fepor_core::settings::SettingsHandle;
use fepor_input::Controls;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

/// A held navigation key repeats like a keyboard's: after this delay, then at this interval.
const REPEAT_DELAY: Duration = Duration::from_millis(400);
const REPEAT_EVERY: Duration = Duration::from_millis(70);
/// Strength at which a stick counts as pushed (it counts as released below half of it).
const STICK_ON: f32 = 0.6;

#[derive(Default)]
struct Queue {
    events: Vec<egui::Event>,
    pointer: Option<egui::Pos2>,
    /// Gamepad inputs currently held as navigation keys, with the time of their next repeat.
    held: HashMap<&'static str, (egui::Key, Instant)>,
}

pub struct Shared {
    open: AtomicBool,
    queue: Mutex<Queue>,
    /// Pending tab change requested by the shoulder buttons or Page Up / Page Down.
    tab_step: AtomicI32,
    pub settings: SettingsHandle,
    pub controls: Arc<Mutex<Controls>>,
    pub data_dir: PathBuf,
    /// The memory card in slot A, once the game opened it.
    pub card_path: Mutex<Option<PathBuf>>,
}

fn key_of(code: KeyCode) -> Option<egui::Key> {
    Some(match code {
        KeyCode::ArrowUp => egui::Key::ArrowUp,
        KeyCode::ArrowDown => egui::Key::ArrowDown,
        KeyCode::ArrowLeft => egui::Key::ArrowLeft,
        KeyCode::ArrowRight => egui::Key::ArrowRight,
        KeyCode::Enter | KeyCode::NumpadEnter => egui::Key::Enter,
        KeyCode::Space => egui::Key::Space,
        KeyCode::Tab => egui::Key::Tab,
        KeyCode::Home => egui::Key::Home,
        KeyCode::End => egui::Key::End,
        _ => return None,
    })
}

/// Gamepad input name to the navigation key it stands for while the overlay is open.
fn nav_key(source: &str) -> Option<(&'static str, egui::Key)> {
    Some(match source {
        "dpup" | "lefty-" => ("up", egui::Key::ArrowUp),
        "dpdown" | "lefty+" => ("down", egui::Key::ArrowDown),
        "dpleft" | "leftx-" => ("left", egui::Key::ArrowLeft),
        "dpright" | "leftx+" => ("right", egui::Key::ArrowRight),
        "a" => ("accept", egui::Key::Enter),
        _ => return None,
    })
}

impl Shared {
    pub fn new(
        settings: SettingsHandle,
        controls: Arc<Mutex<Controls>>,
        data_dir: PathBuf,
    ) -> Arc<Shared> {
        Arc::new(Shared {
            open: AtomicBool::new(false),
            queue: Mutex::new(Queue::default()),
            tab_step: AtomicI32::new(0),
            settings,
            controls,
            data_dir,
            card_path: Mutex::new(None),
        })
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    /// Open or close the overlay. While it is open the game's pad reads neutral.
    pub fn set_open(&self, open: bool) {
        if self.open.swap(open, Ordering::AcqRel) == open {
            return;
        }
        {
            let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            q.events.clear();
            q.held.clear();
        }
        self.tab_step.store(0, Ordering::Release);
        if let Ok(mut c) = self.controls.lock() {
            if !open {
                c.cancel_remap();
            }
            c.set_suspended(open);
        }
    }

    pub fn toggle(&self) {
        self.set_open(!self.is_open());
    }

    fn remapping(&self) -> bool {
        self.controls
            .lock()
            .map(|c| c.is_remapping())
            .unwrap_or(false)
    }

    fn push(&self, e: egui::Event) {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .events
            .push(e);
    }

    fn key(&self, key: egui::Key, pressed: bool, repeat: bool) {
        self.push(egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat,
            modifiers: Default::default(),
        });
    }

    /// A window event, with the window's scale factor. Returns whether the overlay took it: when it did,
    /// the game must not see it.
    pub fn window_event(&self, event: &WindowEvent, scale: f64) -> bool {
        let open = self.is_open();
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        repeat,
                        ..
                    },
                ..
            } => {
                let pressed = *state == ElementState::Pressed;
                if *code == KeyCode::F1 {
                    if pressed && !repeat {
                        self.toggle();
                    }
                    return true;
                }
                // a key being captured as a binding goes to the controls, not to the menu; Escape also
                // abandons a gamepad capture (the keyboard cannot be captured then)
                if let Some((keyboard, _)) =
                    self.controls.lock().ok().and_then(|c| c.remap_target())
                {
                    if *code == KeyCode::Escape && !keyboard && pressed {
                        if let Ok(mut c) = self.controls.lock() {
                            c.cancel_remap();
                        }
                        return true;
                    }
                    return false;
                }
                if !open {
                    return false;
                }
                match code {
                    KeyCode::Escape => {
                        if pressed && !repeat {
                            self.set_open(false);
                        }
                    }
                    KeyCode::PageUp | KeyCode::PageDown => {
                        if pressed {
                            let step = if *code == KeyCode::PageUp { -1 } else { 1 };
                            self.tab_step.fetch_add(step, Ordering::AcqRel);
                        }
                    }
                    c => {
                        if let Some(k) = key_of(*c) {
                            self.key(k, pressed, *repeat);
                        }
                    }
                }
                true
            }
            WindowEvent::CursorMoved { position, .. } if open => {
                let p = egui::pos2((position.x / scale) as f32, (position.y / scale) as f32);
                let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                q.pointer = Some(p);
                q.events.push(egui::Event::PointerMoved(p));
                false
            }
            WindowEvent::CursorLeft { .. } if open => {
                let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                q.pointer = None;
                q.events.push(egui::Event::PointerGone);
                false
            }
            WindowEvent::MouseInput { state, button, .. } if open => {
                let b = match button {
                    MouseButton::Left => egui::PointerButton::Primary,
                    MouseButton::Right => egui::PointerButton::Secondary,
                    MouseButton::Middle => egui::PointerButton::Middle,
                    _ => return true,
                };
                let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(pos) = q.pointer {
                    q.events.push(egui::Event::PointerButton {
                        pos,
                        button: b,
                        pressed: *state == ElementState::Pressed,
                        modifiers: Default::default(),
                    });
                }
                true
            }
            WindowEvent::MouseWheel { delta, .. } if open => {
                let (unit, d) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (egui::MouseWheelUnit::Line, egui::vec2(*x, *y))
                    }
                    MouseScrollDelta::PixelDelta(p) => (
                        egui::MouseWheelUnit::Point,
                        egui::vec2((p.x / scale) as f32, (p.y / scale) as f32),
                    ),
                };
                self.push(egui::Event::MouseWheel {
                    unit,
                    delta: d,
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers: Default::default(),
                });
                true
            }
            _ => false,
        }
    }

    /// A gamepad input (SDL's name, strength 0..1). Returns whether the overlay took it. The guide or back
    /// button opens and closes the overlay.
    pub fn gamepad_input(&self, source: &str, value: f32) -> bool {
        if self.remapping() {
            return false;
        }
        let pressed = value >= STICK_ON;
        if matches!(source, "guide" | "back") {
            if pressed {
                self.toggle();
            }
            return true;
        }
        if !self.is_open() {
            return false;
        }
        match source {
            "b" => {
                if pressed {
                    self.set_open(false);
                }
            }
            "leftshoulder" | "rightshoulder" => {
                if pressed {
                    let step = if source == "leftshoulder" { -1 } else { 1 };
                    self.tab_step.fetch_add(step, Ordering::AcqRel);
                }
            }
            s => {
                if let Some((name, key)) = nav_key(s) {
                    let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                    let down = q.held.contains_key(name);
                    if pressed && !down {
                        q.held.insert(name, (key, Instant::now() + REPEAT_DELAY));
                        q.events.push(egui::Event::Key {
                            key,
                            physical_key: Some(key),
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        });
                    } else if value < STICK_ON / 2.0 && down {
                        q.held.remove(name);
                        q.events.push(egui::Event::Key {
                            key,
                            physical_key: Some(key),
                            pressed: false,
                            repeat: false,
                            modifiers: Default::default(),
                        });
                    }
                }
            }
        }
        true
    }

    /// The events to give egui for this frame (including key repeats of held pad directions).
    pub(crate) fn take_events(&self) -> Vec<egui::Event> {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let mut out = std::mem::take(&mut q.events);
        for (key, next) in q.held.values_mut() {
            if now >= *next {
                *next = now + REPEAT_EVERY;
                out.push(egui::Event::Key {
                    key: *key,
                    physical_key: Some(*key),
                    pressed: true,
                    repeat: true,
                    modifiers: Default::default(),
                });
            }
        }
        out
    }

    pub(crate) fn take_tab_step(&self) -> i32 {
        self.tab_step.swap(0, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fepor_input::Action;

    fn shared() -> (Arc<Shared>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let controls = Arc::new(Mutex::new(Controls::new(dir.path().join("input.toml"))));
        (
            Shared::new(SettingsHandle::default(), controls, dir.path().into()),
            dir,
        )
    }

    #[test]
    fn guide_toggles_and_pad_navigates_only_while_open() {
        let (s, _dir) = shared();
        assert!(
            !s.gamepad_input("dpdown", 1.0),
            "the game gets the pad while the overlay is closed"
        );
        assert!(s.gamepad_input("guide", 1.0));
        assert!(s.is_open());
        assert!(s.gamepad_input("dpdown", 1.0));
        let keys: Vec<_> = s.take_events();
        assert!(matches!(
            keys[..],
            [egui::Event::Key {
                key: egui::Key::ArrowDown,
                pressed: true,
                ..
            }]
        ));
        assert!(s.gamepad_input("dpdown", 0.0));
        assert!(s.gamepad_input("b", 1.0));
        assert!(!s.is_open());
    }

    #[test]
    fn a_pad_input_being_captured_as_a_binding_is_left_to_the_controls() {
        let (s, _dir) = shared();
        s.set_open(true);
        s.controls.lock().unwrap().rebind(false, Action::A);
        assert!(
            !s.gamepad_input("x", 1.0),
            "captured inputs must reach the controls"
        );
        assert!(!s.gamepad_input("guide", 1.0));
        assert!(s.is_open());
    }

    #[test]
    fn closing_abandons_a_capture() {
        let (s, _dir) = shared();
        s.set_open(true);
        s.controls.lock().unwrap().rebind(true, Action::A);
        s.set_open(false);
        assert!(!s.controls.lock().unwrap().is_remapping());
    }
}
