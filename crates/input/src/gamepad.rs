//! SDL3 gamepad reader. Runs on its own thread and reports gamepads being plugged in and out and every input
//! as `(device, source name, strength)`.

use std::collections::HashMap;
use std::time::Duration;

use sdl3::event::Event as SdlEvent;
use sdl3::gamepad::{Axis, Gamepad};
use sdl3::GamepadSubsystem;

use crate::DeviceInfo;

/// What the reader reports.
pub enum Event<'a> {
    /// A gamepad is there: found at start, or plugged in while the game runs.
    Added(DeviceInfo),
    /// The gamepad with this id was unplugged.
    Removed(u32),
    /// An input of a gamepad, with SDL's names (`a`, `dpup`, `leftx+`, `lefttrigger`, ...) and a strength
    /// between 0 and 1.
    Input {
        id: u32,
        source: &'a str,
        value: f32,
    },
}

fn info(subsystem: &GamepadSubsystem, id: u32) -> DeviceInfo {
    DeviceInfo {
        id,
        guid: subsystem.guid_for_id(id).string(),
        name: subsystem.name_for_id(id).unwrap_or_default(),
    }
}

/// Start the reader. `on_event` is called from the reader thread.
pub fn spawn(mut on_event: impl FnMut(Event<'_>) + Send + 'static) {
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
            let add =
                |id: u32, open: &mut HashMap<u32, Gamepad>, on_event: &mut dyn FnMut(Event<'_>)| {
                    if let Ok(g) = subsystem.open(id) {
                        if trace {
                            eprintln!("input: gamepad connected: {}", g.name().unwrap_or_default());
                        }
                        open.insert(id, g);
                        on_event(Event::Added(info(&subsystem, id)));
                    }
                };
            if let Ok(ids) = subsystem.gamepads() {
                for id in ids {
                    add(id, &mut open, &mut on_event);
                }
            }
            loop {
                for event in pump.poll_iter() {
                    match event {
                        SdlEvent::ControllerDeviceAdded { which, .. } => {
                            add(which, &mut open, &mut on_event)
                        }
                        SdlEvent::ControllerDeviceRemoved { which, .. } => {
                            if open.remove(&which).is_some() {
                                if trace {
                                    eprintln!("input: gamepad {which} disconnected");
                                }
                                on_event(Event::Removed(which));
                            }
                        }
                        SdlEvent::ControllerButtonDown { which, button, .. } => {
                            if trace {
                                eprintln!("input: gamepad button {} down", button.string());
                            }
                            on_event(Event::Input {
                                id: which,
                                source: &button.string(),
                                value: 1.0,
                            })
                        }
                        SdlEvent::ControllerButtonUp { which, button, .. } => {
                            on_event(Event::Input {
                                id: which,
                                source: &button.string(),
                                value: 0.0,
                            })
                        }
                        SdlEvent::ControllerAxisMotion {
                            which, axis, value, ..
                        } => {
                            let v = value as f32 / 32767.0;
                            let name = axis.string();
                            let mut report = |source: &str, value: f32| {
                                on_event(Event::Input {
                                    id: which,
                                    source,
                                    value,
                                })
                            };
                            match axis {
                                Axis::TriggerLeft | Axis::TriggerRight => report(&name, v.max(0.0)),
                                _ => {
                                    report(&format!("{name}+"), v.max(0.0));
                                    report(&format!("{name}-"), (-v).max(0.0));
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

#[cfg(test)]
mod tests {
    use super::*;
    use sdl3::sys::gamepad::{
        SDL_GAMEPAD_AXIS_COUNT, SDL_GAMEPAD_BUTTON_COUNT, SDL_GAMEPAD_BUTTON_SOUTH,
    };
    use sdl3::sys::joystick::*;
    use std::sync::mpsc;

    /// Plug a virtual gamepad into SDL, press its south button, unplug it: the reader reports each step.
    /// No hardware involved: SDL's virtual joystick goes through the same device and event machinery as a
    /// USB pad does.
    #[test]
    fn a_virtual_gamepad_is_reported_plugged_used_and_unplugged() {
        let (tx, rx) = mpsc::channel::<String>();
        spawn(move |e| {
            let _ = tx.send(match e {
                Event::Added(d) => format!("added {} {}", d.id, d.name),
                Event::Removed(id) => format!("removed {id}"),
                Event::Input { id, source, value } => format!("input {id} {source} {value}"),
            });
        });
        let sdl = sdl3::init().unwrap();
        let _joysticks = sdl.joystick().unwrap();
        let mut desc = SDL_VirtualJoystickDesc::new();
        desc.r#type = SDL_JOYSTICK_TYPE_GAMEPAD.0 as u16;
        desc.naxes = SDL_GAMEPAD_AXIS_COUNT.0 as u16;
        desc.nbuttons = SDL_GAMEPAD_BUTTON_COUNT.0 as u16;
        desc.button_mask = (1 << SDL_GAMEPAD_BUTTON_COUNT.0) - 1;
        desc.axis_mask = (1 << SDL_GAMEPAD_AXIS_COUNT.0) - 1;
        desc.name = c"Virtual test pad".as_ptr();
        let id = unsafe { SDL_AttachVirtualJoystick(&desc) };
        assert_ne!(id, 0, "{}", sdl3::get_error());

        // a real pad may be plugged in on the machine running the test: its events are skipped
        let wait_for = |want: &str| {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            let mut seen = Vec::new();
            while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(m) if m == want => return m,
                    Ok(m) => seen.push(m),
                    Err(_) => break,
                }
            }
            panic!("no {want:?}; saw {seen:?}");
        };
        wait_for(&format!("added {id} Virtual test pad"));

        let joystick = unsafe { SDL_OpenJoystick(id) };
        assert!(!joystick.is_null(), "{}", sdl3::get_error());
        unsafe { SDL_SetJoystickVirtualButton(joystick, SDL_GAMEPAD_BUTTON_SOUTH.0, true) };
        wait_for(&format!("input {id} a 1"));

        unsafe {
            SDL_CloseJoystick(joystick);
            SDL_DetachVirtualJoystick(id);
        }
        wait_for(&format!("removed {id}"));
    }
}
