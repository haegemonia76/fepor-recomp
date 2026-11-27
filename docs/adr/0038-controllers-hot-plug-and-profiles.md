# 0038 Controllers: hot-plug and a profile per model

Status: accepted (amends ADR 0016 and ADR 0020, implements ticket #38)

**What**
- Gamepads can be plugged in and out while the game runs. A notice ("Gamepad connected: ...") is drawn over the game for five seconds (the overlay paints it even when closed) and put in the window title. Nothing needs pressing: the pad works at once.
- Every controller model has its own profile in `input.toml`: `[profiles.<SDL GUID>]` with `name`, `glyphs` (button-name family) and `[profiles.<GUID>.gamepad]` (input name -> GameCube action). The profile is selected automatically from the GUID of the pad; the first time a model is seen its profile is copied from the shared `[bindings.gamepad]` and top-level `glyphs`, which stay in the file as the template for new models (and apply while no pad is plugged in).
- Backward compatibility: files written before profiles have no `[profiles]`; they load unchanged (every field defaults) and their `[bindings.gamepad]`/`glyphs` become the profile of the first pad that is plugged in, so a single pad behaves exactly as before. Keyboard bindings and the dead zone stay global (the dead zone and the trigger thresholds are ticket #27's).
- **One pad plays at a time.** This game is single-player and reads controller port 1 only, so there is no multi-port support (an earlier version of this change had players 2 to 4; it was dropped as dead weight, the SI still answers on port 1 only). The keyboard and the active gamepad both feed port 1. With several pads plugged in, the active one is **the pad plugged in last or pressed last**: a pad that is pressed (input at 0.6 or more) takes over, what the previous one held is let go, and the others are ignored until they are pressed. When the active pad is unplugged, the first remaining one takes over.
- The overlay edits the profile of the pad picked in the Gamepads list (default: the active pad); rebinding, glyph family, restore defaults and the drawing all act on that profile. The icons the game draws on the map follow the profile of the active pad. In the list each pad shows its own name and, beside it, a short label for its profile: `Profile of this model`, `Profile shared by N pads` when several pads of one model are plugged in, or `Profile 1`, `Profile 2`... (order of first plug) when the pad reports no name (it is then called "Unknown gamepad"); hovering the label shows `Profile: <model name>` and the GUID.
- Overlay with keyboard and mouse: arrows/Enter/Space/Tab/Home/End as before, now with Shift+Tab (modifier keys are tracked), Page Up/Down page through the tabs, and so do the mouse Back/Forward buttons.

**How**
- `crates/input`: `gamepad::spawn` reports `Event::{Added(DeviceInfo), Removed(id), Input{id, source, value}}` (SDL gamepad added/removed events, GUID from `SDL_GetGamepadGUIDForID`). A failed open is logged, and a device name that is not UTF-8 (the sdl3 crate unwraps it) falls back to "Unknown gamepad" instead of killing the reader thread. `Controls::device_added/device_removed/pad_input/keyboard_input` are the whole interface of the SDL layer, so the logic is unit-tested without hardware.
- `FEPOR_FAKE_PADS=<name>[,<name>...]` plugs in pretend pads at start (headless runs, screenshots, profile tests); documented in `tools/README.md`.

**Not done / limits**
- Rumble and dead zone/trigger thresholds: ticket #27.
- Hot-plug was tested with SDL's virtual joystick, the fake pads and, by the maintainer, a real 8BitDo pad.
