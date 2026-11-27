//! SDL3 gamepad reader. Runs on its own thread and reports every input as `(source name, strength)`.

use std::collections::HashMap;
use std::time::Duration;

use sdl3::event::Event;
use sdl3::gamepad::{Axis, Gamepad};

/// Tell the bug report which controllers are connected (name, GUID, player index).
fn note_pads(subsystem: &sdl3::GamepadSubsystem, open: &HashMap<u32, Gamepad>) {
    let mut t = format!("SDL {}\n", sdl3::version::version());
    if open.is_empty() {
        t.push_str("no controller detected\n");
    }
    let mut ids: Vec<&u32> = open.keys().collect();
    ids.sort();
    for id in ids {
        t.push_str(&format!(
            "controller: {} (GUID {}, player index {:?})\n",
            open[id].name().unwrap_or_default(),
            subsystem.guid_for_id(*id).string(),
            subsystem.player_index_for_id(*id)
        ));
    }
    fepor_core::bugreport::note("controllers", t);
}

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
            let trace = std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "input" || x == "all"));
            let mut open: HashMap<u32, Gamepad> = HashMap::new();
            if let Ok(ids) = subsystem.gamepads() {
                for id in ids {
                    if let Ok(g) = subsystem.open(id) {
                        if trace {
                            eprintln!("input: gamepad found: {}", g.name().unwrap_or_default());
                        }
                        open.insert(id, g);
                    }
                }
            }
            note_pads(&subsystem, &open);
            loop {
                for event in pump.poll_iter() {
                    match event {
                        Event::ControllerDeviceAdded { which, .. } => {
                            if let Ok(g) = subsystem.open(which) {
                                if trace {
                                    eprintln!(
                                        "input: gamepad connected: {}",
                                        g.name().unwrap_or_default()
                                    );
                                }
                                open.insert(which, g);
                                note_pads(&subsystem, &open);
                            }
                        }
                        Event::ControllerDeviceRemoved { which, .. } => {
                            open.remove(&which);
                            note_pads(&subsystem, &open);
                        }
                        Event::ControllerButtonDown { button, .. } => {
                            if trace {
                                eprintln!("input: gamepad button {} down", button.string());
                            }
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
