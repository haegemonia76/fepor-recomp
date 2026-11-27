# fepor-recomp

Static recompilation of *Fire Emblem: Path of Radiance* (GameCube, EU build `GFEP01`) into a native desktop program (Linux, Windows, macOS; x64 and arm64). The user supplies their own disc image; this repo ships only tooling and runtime, never Nintendo-derived content.

## Glossary

- **Guest**: the original GameCube software and hardware model (Gekko PPC, Flipper GPU, DSP). **Host**: the machine running the recomp.
- **Disc image**: user-supplied ISO/GCM, RVZ or GCZ (or extracted folder). Read through an abstract reader (`crates/core/src/image.rs`) so more formats can be added.
- **DOL**: `main.dol`, the single executable of the game. No `.rel` modules exist, so all guest code is at fixed addresses.
- **FST**: the disc file system table.
- **Lifter**: Rust component turning Gekko instructions into C++ (`crates/ppc-lift`).
- **Generated code**: C++ emitted by the lifter, grouped into configurable compilation units. Always regenerated locally, never committed.
- **Runtime**: hand-written C++ in `runtime/` (os, dvd, gx, ax, input, ui) that the generated code links against.
- **Context**: the guest CPU state struct (`runtime/include/fepor/ppc.h`). Generated functions copy registers into locals with `in()` and back with `out()` (ADR 0009).
- **Dispatch table**: runtime map from guest address to host function, used for indirect calls (`bctr`, `bctrl`, callbacks, vtables) that static analysis cannot resolve. A miss is an explicit error with the address logged.
- **HLE**: high-level emulation. Guest SDK functions (OS, DVD, PAD, CARD, AX, VI) replaced by native C++.
- **LLE**: low-level emulation. Used only for the GX FIFO: guest code writes to the write-gather pipe and the runtime parses the CP/XF/BP command stream.
- **GX backend**: renderer built on SDL_GPU consuming the parsed FIFO. Renders into a resizable virtual EFB.
- **Virtual pad**: GameCube controller abstraction fed by the input layer (remapping, glyph sets Xbox/Nintendo/PlayStation). Up to four, one per GameCube **port**; each plugged-in gamepad drives one port through the **profile** of its model (bindings and glyph set keyed by SDL GUID in `input.toml`), the keyboard plays port 1.
- **Fiber**: host stack backing one guest `OSThread`; switched by the cooperative deterministic scheduler.
- **Start / Origin**: a function start found by analysis and why (`Entry`, `Config`, `Call`, `TailCall`, `DataPointer`, `AddressLoad`, `Gap`). Untrusted origins are rejected when preceding code runs straight into them (mid-function labels).
- **Data folder**: the single directory (`--data`, `RECOMPLAUNCHER_DATA`) where generated code, the build, saves, config, cache and logs live (ADR 0010).
- **Prepare**: the setup step (`fepor-recomp prepare`) that builds the game from the player's copy; **profile**: `packaging/recomplauncher.json`, the RecompLauncher integration file.
- **Config**: shared TOML file read by CLI and GUI (disc path, output dir, function/jump-table overrides, runtime options).
- **Trace**: opt-in (`FEPOR_TRACE`) log of guest calls, hardware register writes and GX commands, comparable with Dolphin.
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
