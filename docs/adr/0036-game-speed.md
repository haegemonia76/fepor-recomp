# 0036 Fast-forward, slow motion and the guest clock

Status: accepted (ticket #36)

## Context

Tickets and players ask for a fast-forward key (skip slow text and animations, grind) and a slow motion
(inspect an animation). The recomp paces itself on the host's wall clock: the retrace interrupt
(`Hw::vi_tick`), the audio DMA (`Dsp::tick`), the audio sample counter (`Ai`) and the time base
(`Machine::timebase`) each read `Instant::now()`. Speeding up only one of them (retraces alone, say) would
leave the game's alarms, the decrementer and the sound mixer at real time and break the game's own timing.

## Decisions

- **One clock.** `fepor_runtime::clock::now_ns()` is real time scaled by the speed factor and is the only time
  source of the machine. A change of the factor starts a new segment, so the clock never jumps. Everything the
  game can observe (time base, decrementer, retrace, AI sample counter, audio buffers) therefore runs at the
  same speed and the game cannot tell. The clock is process-wide because guest threads run on their own host
  threads.
- **Speeds.** `fepor_core::speed`: x0.25, x0.5, x1, x2, x4 and uncapped. The factor is one process-wide atomic.
  Uncapped has no fixed factor: it starts at x4 and `Hw::vi_steer` tells `speed::adapt` four times a second
  whether the guest kept up (no retrace came due while the guest was busy, and the game drew at least 0.4
  frames per retrace; a game that draws nothing does not count). Lagging lowers the factor by a quarter, keeping
  up raises it by 15 %, up to x32. A fixed x16 hung the boot on the test machine (every guest thread waiting,
  no frame drawn), so a fixed "very large" factor is not an option.
- **Controls.** Held, not toggled, so a forgotten state cannot stay on: Tab or the pressed right stick is
  fast-forward (`game.fast_forward` in `settings.toml`: x2, x4 or uncapped; default x4), the backtick or the
  pressed left stick is slow motion (`game.slow_motion`: x0.5 or x0.25). The overlay has a Speed page for both
  and for the sound. The keys are handled by the overlay's input (`crates/overlay/src/speed.rs`) like F1 and F2, before
  the game's bindings, and are not bindable GameCube buttons (a GameCube pad has no stick buttons, so the
  gamepad defaults do not use them). Losing the window focus or opening the overlay lets go. `FEPOR_SPEED=0.25|0.5|2|4|max`
  starts a run at that speed (headless measurements) and is not saved.
- **Sound policy** (`audio.speed`, `crates/audio` `SpeedPolicy`). The game produces its samples at the speed's
  pace, so *pitch* (default) plays them at `rate x factor`: the stream stays continuous, only the pitch and the
  tempo follow the speed (tape-like), with no resampling, dropping or stretching, hence no glitches. The SDL
  stream is reopened when the rate changes, so switching speed has one short gap. *Silent* plays nothing at other
  speeds. Uncapped is always silent: the buffers arrive faster than a device can play and the rate would be
  far beyond what devices accept.
- **Rendering.** While faster than real time the renderer does not interpolate (`frame_multiplier` is ignored,
  nothing is recorded), uses a present mode that never waits for the display (`Presenter::set_present`) and
  shows a frame at most every 8 ms: waiting for vsync would hold the guest thread at the display's rate.
- **Measurement.** `FEPOR_TRACE=vi` prints once a second the retraces and the game frames per real second and
  the factor; the statistics HUD shows the factor when it is not x1.

## Battle animations

The game has its own switch: the options menu (`Mess/common.m`, "Animation: Adjust combat animation", "Turn
combat animations on or off", next to "Combat Camera" and "Game Speed" Slow / Normal / Fast / Max). No native
skip toggle was added: the guest variable behind it could not be found. The disc has no symbols, the
options are only reachable in a played chapter, and the scripted runs that were tried (`FEPOR_PAD` through the
prologue with the maintainer's card, at x4) stalled on a dialogue that did not accept the scripted A presses,
so no RAM could be compared with the option toggled (`FEPOR_WATCH_MEM` needs the address first). Meanwhile
fast-forward plus the game's own "Animation: Off" and "Game Speed: Max" cover the use.
To continue: reach the options menu with a script, dump guest RAM before and after toggling "Animation" (a
word that flips between two values), then `FEPOR_TRACE=calls FEPOR_WATCH_MEM=<address>` shows the writer and
the reader of the flag, and a native toggle can write it like `Hw::set_language` writes the SRAM.

## Limits

- The game frame rate at a speed is capped by the host: scenes the game renders at 60 frames per second need
  the host to run them at x4 = 240 per second. Where the host cannot, the game's frames are dropped, not its logic.
- Pad scripts (`FEPOR_PAD`) count retraces, which follow the guest clock, so a script behaves the same at any speed.
