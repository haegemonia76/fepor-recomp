# fepor-recomp

Static recompilation of *Fire Emblem: Path of Radiance* (GameCube, EU build `GFEP01`) into a native desktop program (Linux, Windows, macOS; x64 and arm64). The user supplies their own disc image; this repo ships only tooling and runtime, never Nintendo-derived content.

## Glossary

- **Guest**: the original GameCube software and hardware model (Gekko PPC, Flipper GPU, DSP). **Host**: the machine running the recomp.
- **Disc image**: user-supplied ISO/GCM, RVZ or GCZ (or extracted folder). Read through an abstract reader (`crates/core/src/image.rs`) so more formats can be added.
- **DOL**: `main.dol`, the single executable of the game. No `.rel` modules exist, so all guest code is at fixed addresses.
- **FST**: the disc file system table.
- **Lifter**: Rust component turning Gekko instructions into C++ (`crates/ppc-lift`).
- **Patch set**: `(address, bytes)` writes over guest RAM owned by one source (cheat, mod), enabled/reverted as a unit by `PatchEngine`, which also runs the once-per-frame hook (ADR 0025). Data only: recompiled code cannot be patched.
- **Mod**: folder `<data>/mods/<id>/` with `mod.toml`, replacement `files/` and `[[patch]]` memory patches; priority order and enabled state in `mods/mods.toml`; applied at the next start. File replacement is `ModdedDisc`: replaced files appended after the image end, FST rewritten (ADR 0034). Never contains Nintendo content.
- **Memory watcher**: developer page of the overlay to read, write, watch and search guest RAM and export an address as a cheat entry or Gecko line (ADR 0048). The overlay never touches RAM: requests and copies pass through `memwatch::Link`, served in the per-frame hook.
- **Cheat**: a Gecko or Action Replay code of a Dolphin game INI, run as a continuous patch set with source `cheat:<name>` by `CheatRunner`; only plain data writes are supported (ADR 0032). Enabled ones are saved in `<data>/cheats.toml`.
- **Generated code**: C++ emitted by the lifter, grouped into configurable compilation units. Always regenerated locally, never committed.
- **Runtime**: hand-written C++ in `runtime/` (os, dvd, gx, ax, input, ui) that the generated code links against.
- **Context**: the guest CPU state struct (`runtime/include/fepor/ppc.h`). Generated functions copy registers into locals with `in()` and back with `out()` (ADR 0009).
- **Dispatch table**: runtime map from guest address to host function, used for indirect calls (`bctr`, `bctrl`, callbacks, vtables) that static analysis cannot resolve. A miss is an explicit error with the address logged.
- **HLE**: high-level emulation. Guest SDK functions (OS, DVD, PAD, CARD, AX, VI) replaced by native C++.
- **LLE**: low-level emulation. Used only for the GX FIFO: guest code writes to the write-gather pipe and the runtime parses the CP/XF/BP command stream.
- **GX backend**: renderer built on SDL_GPU consuming the parsed FIFO. Renders into a resizable virtual EFB.
- **Virtual pad**: GameCube controller abstraction fed by the input layer (remapping, glyph sets Xbox/Nintendo/PlayStation).
- **Render filters**: post-process of the presented image only (scanlines, gamma, saturation/contrast/brightness, screen mask); `[video.filters]` in `settings.toml`, Video page of the overlay, never applied to the EFB or the overlay (ADR 0039).
- **MSAA / anisotropy**: `[video] msaa` (1/2/4/8) multisamples the internal render targets, resolved at the end of every pass so the EFB, its copies and the presenter only ever see single-sample pixels; `[video] anisotropy` (1-16) sets the clamp of the guest texture samplers. Both apply live, fall back to what the GPU offers (ADR 0041).
- **FSR**: AMD FidelityFX Super Resolution 1 (EASU upscale, RCAS sharpening) in the presentation pass, before the render filters and never on the overlay; `[video.fsr]` in `settings.toml`, Video page of the overlay; while on it chooses the internal resolution from a quality preset (ADR 0042).
- **Fiber**: host stack backing one guest `OSThread`; switched by the cooperative deterministic scheduler.
- **Start / Origin**: a function start found by analysis and why (`Entry`, `Config`, `Call`, `TailCall`, `DataPointer`, `AddressLoad`, `Gap`). Untrusted origins are rejected when preceding code runs straight into them (mid-function labels).
- **Data folder**: the single directory (`--data`, `RECOMPLAUNCHER_DATA`) where generated code, the build, saves, config, cache and logs live (ADR 0010).
- **Playtime**: seconds the guest ran with the overlay closed, per game id, in `<data>/playtime.toml` (ADR 0045). **Presence**: opt-in Discord rich presence over local IPC (ADR 0045).
- **Prepare**: the setup step (`fepor-recomp prepare`) that builds the game from the player's copy; **profile**: `packaging/recomplauncher.json`, the RecompLauncher integration file.
- **Config**: shared TOML file read by CLI and GUI (disc path, output dir, function/jump-table overrides, runtime options).
- **Trace**: opt-in (`FEPOR_TRACE`) log of guest calls, hardware register writes and GX commands, comparable with Dolphin.
- **Message file**: `Mess/*.m` on the disc, the game's text (names, dialogue, menus) per chapter and language. `text-extract` / `text-build` convert it to an editable form and to a mod file (ADR 0040).
- **Version table**: per-region address tables; only GFEP01 at first.

## Layout (planned)

```
crates/core      disc reading, DOL/FST parsing, analysis, config
crates/ppc-lift  Gekko decode (ppc750cl) + C++ emission
crates/cli       fepor-recomp subcommands: extract, analyze, generate (build, run later)
crates/gui       egui front-end over core
runtime/         C++/cmake runtime
docs/adr/        decisions
```

## Milestones

1. DOL/FST extraction, Gekko disassembly, generated C++ that compiles; instruction tests.
2. Minimal runtime boots (OS init, memory, VI retrace, black screen).
3. GX: FIFO, TEV, Nintendo logo.
4. DVD HLE, title menu (user's "milestone 1").
5. Audio (AX HLE) and input with remapping.
6. Extras (high fps, resolution) and first playable stage.
