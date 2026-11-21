//! SDL3 gamepad reader. Runs on its own thread and reports every input as `(source name, strength)`.

use std::collections::HashMap;
use std::time::Duration;

use sdl3::event::Event;
use sdl3::gamepad::{Axis, Gamepad};

/// Start the reader. `on_input` is called from the reader thread with SDL's input names (`a`, `dpup`,
/// `leftx+`, `lefttrigger`, ...) and a strength between 0 and 1.
pub fn spawn(mut on_input: impl FnMut(&str, f32) + Send + 'static) {
    let _ = std::thread::Builder::new()
        .name("gamepad".into())
        .spawn(move || {
            sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
            let Ok(sdl) = sdl3::init() else { return };
            let Ok(subsystem) = sdl.gamepad() else { return };
            let Ok(mut pump) = sdl.event_pump() else {
                return;
            };
            let mut open: HashMap<u32, Gamepad> = HashMap::new();
            if let Ok(ids) = subsystem.gamepads() {
                for id in ids {
                    if let Ok(g) = subsystem.open(id) {
                        open.insert(id, g);
                    }
                }
            }
            loop {
                for event in pump.poll_iter() {
                    match event {
                        Event::ControllerDeviceAdded { which, .. } => {
                            if let Ok(g) = subsystem.open(which) {
                                open.insert(which, g);
                            }
                        }
                        Event::ControllerDeviceRemoved { which, .. } => {
                            open.remove(&which);
                        }
                        Event::ControllerButtonDown { button, .. } => {
                            on_input(&button.string(), 1.0)
                        }
                        Event::ControllerButtonUp { button, .. } => on_input(&button.string(), 0.0),
                        Event::ControllerAxisMotion { axis, value, .. } => {
                            let v = value as f32 / 32767.0;
                            let name = axis.string();
                            match axis {
                                Axis::TriggerLeft | Axis::TriggerRight => {
                                    on_input(&name, v.max(0.0))
                                }
                                _ => {
                                    on_input(&format!("{name}+"), v.max(0.0));
                                    on_input(&format!("{name}-"), (-v).max(0.0));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
}
