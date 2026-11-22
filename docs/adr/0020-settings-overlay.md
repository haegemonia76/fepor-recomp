# 0020 In-game settings overlay

Status: accepted (implements ticket #20, replaces the F2/F3/F4 hotkeys of ADR 0016)

**What**
- All player-facing options live in an overlay drawn over the game window: Video (frame multiplier, internal resolution, picture shape, fullscreen), Audio (master volume), Controls (per-action rebinding for keyboard and gamepad, button-name family, stick dead zone, restore defaults), Memory card (slot A status, saves, export as `.gci`) and Help. It is opened with F1 or the gamepad's Guide / Back button and closed with the same, Esc or B. Page Up/Down or LB/RB switch pages; navigation is arrows + Enter, D-pad / left stick + A, or the mouse.
- While it is open the game's pad reads neutral (`Controls::set_suspended`); inputs being captured as a binding bypass the menu and go to `Controls`.

**How**
- `crates/overlay` uses egui (`egui` core only, default fonts) for layout and our own wgpu painter (`paint.rs`, a small port of egui-wgpu's renderer) because `egui-wgpu` needs a newer wgpu than `fepor-video`. Events are converted from winit by hand (`shared.rs`): the window thread and the gamepad thread push into `Shared`, the render thread owns `Overlay`, runs the UI once per presented image and paints it as a last render pass over the swapchain (`Presenter::show`). Held pad directions repeat like a keyboard.
- Settings: `fepor_core::settings` (`<data>/settings.toml`: `[video] frame_multiplier, resolution, aspect, fullscreen`, `[audio] volume`). A `SettingsHandle` is shared by the overlay (writer, saves on change), the renderer and the audio sink (readers, poll `generation()`). `FEPOR_FPS_MULT`, `FEPOR_RES`, `FEPOR_ASPECT` still override the file for one run and are never saved.
- Live application: frame multiplier (drops the recorded frames, switches the present mode), aspect, fullscreen and volume apply at once; resolution rebuilds the render targets between frames (`Renderer::apply_settings`). The volume gain is the slider squared.
- Memory cards: the page is read-only plus export. Saves cannot be imported into a card the running game has open, so `.gci` files put in `<data>/imports` are imported into slot A at the next start (renamed `.gci.imported`); the card must already be formatted by the game.
- Debugging: `FEPOR_OVERLAY=<page>` opens the overlay at start; with `FEPOR_HEADLESS=1 FEPOR_DUMP_FRAMES=dir` it is painted on the dumped frames.

**Not done**
- Replacing the button icons the game draws (needs textures replaced by content hash).
- Picking an audio device, vsync option, importing a card from a file chooser.
- The overlay is not drawn into frame dumps when interpolating (`frame_N_tXX.png`).
