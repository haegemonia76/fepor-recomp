# 0038 Controllers: hot-plug, profiles per model, players 1 to 4

Status: accepted (amends ADR 0016 and ADR 0020, implements ticket #38)

**What**
- Gamepads can be plugged in and out while the game runs. A notice ("Gamepad connected: ... (player 2)") is drawn over the game for five seconds (the overlay paints it even when closed) and put in the window title. Pressing nothing is needed: the pad works at once.
- Every controller model has its own profile in `input.toml`: `[profiles.<SDL GUID>]` with `name`, `glyphs` (button-name family) and `[profiles.<GUID>.gamepad]` (input name -> GameCube action). The profile is selected automatically from the GUID of the pad; the first time a model is seen its profile is copied from the shared `[bindings.gamepad]` and top-level `glyphs`, which stay in the file as the template for new models (and apply while no pad is plugged in).
- Backward compatibility: files written before profiles have no `[profiles]`; they load unchanged (every field defaults) and their `[bindings.gamepad]`/`glyphs` become the profile of the first pad that is plugged in, so a single pad behaves exactly as before. Keyboard bindings and the dead zone stay global (the dead zone and the trigger thresholds are ticket #27's).
- Up to four pads: each takes the lowest free GameCube port in the order they are plugged in; the Controls page lets the player move a pad to another player (a pad already there swaps). The keyboard always plays player 1 and shares port 1 with a pad assigned to it. A fifth pad is listed without a port and takes the first port that frees up. Port assignments are not saved (they follow the order of plug-in).
- The overlay edits the profile of the pad picked in the Gamepads list (default: the pad on port 1); rebinding, glyph family, restore defaults and the drawing all act on that profile. The icons the game draws on the map follow the profile of the pad on port 1.
- Overlay with keyboard and mouse: arrows/Enter/Space/Tab/Home/End as before, now with Shift+Tab (modifier keys are tracked), Page Up/Down page through the tabs, mouse buttons Back/Forward do the same, left/right click, wheel and drag as before.

**How**
- `crates/input`: `gamepad::spawn` reports `Event::{Added(DeviceInfo), Removed(id), Input{id, source, value}}` (SDL gamepad added/removed events, GUID from `SDL_GetGamepadGUIDForID`). `Controls` keeps the list of pads, one `InputState` per port and publishes each port to the SI (`si::set_pad(port, ..)`, `si::set_port_connected`). `Controls::device_added/device_removed/pad_input/keyboard_input` are the whole interface of the SDL layer, so the logic is unit-tested without hardware.
- `crates/runtime/src/si.rs`: the SI answers on channels 1 to 3 while a pad is reported there (device ID probe, poll status). A polled channel whose pad is unplugged raises the no-response flag (SISR NOREP), as a real console does, which makes the SDK's PAD library notice the removal; a pad plugged in later is found by the SDK's periodic probing.
- `FEPOR_FAKE_PADS=<name>[,<name>...]` plugs in pretend pads at start (headless runs, screenshots); documented in `tools/README.md`.

**Not done / limits**
- Rumble and dead zone/trigger thresholds: ticket #27. Hot-plug was tested with SDL's virtual joystick and with the fake pads, not by unplugging physical hardware, and players 2 to 4 were tested to the point that the game starts and polls them, not in a game mode that uses a second controller (Path of Radiance reads controller 1 only, to our knowledge).
- Ports are not remembered between runs; the profile of a model is.
