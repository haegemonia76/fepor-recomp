# 0017 Launcher GUI

Status: accepted (narrows ticket M5.3)

- `crates/gui` (`fepor-recomp-gui`, eframe/egui) does three things: choose the game file (disc image or extracted folder, checked live: GFEP01 or an explanation), choose the output folder, run the recompilation with a progress bar, a cancel button and the build log. Paths are remembered in `<config>/fepor-recomp/gui.toml`.
- The work is `fepor-recomp prepare --game .. --data .. --build-type Release`, started as a child process (found next to the GUI executable, else on the PATH) so it can be cancelled. `progress <0..1> <text>` lines drive the bar; the exit code (10/11/20/30, as in `packaging/recomplauncher.json`) selects the message.
- No settings live here. All player configuration (controls, glyphs, video, audio, memory card) belongs to an in-game overlay, ticket M5.4.
- `FEPOR_GUI_AUTOSTART=1` starts the recompilation when the window opens (scripted checks).
