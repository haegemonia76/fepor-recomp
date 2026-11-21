# 0002 Toolchain and repo structure

Status: accepted

- Rust workspace: `core`, `ppc-lift`, `cli`, `gui` (egui). CLI and GUI share the core library and one TOML config.
- Decoding via `ppc750cl` (licence and Gekko coverage to verify first); the lifter is ours.
- Runtime in C++ built with cmake, ninja and clang on all three OS, x64 and arm64.
- Symbols: read-only reference to the community `fe9` decomp after a licence check.
- Licence: MIT. No Dolphin code copied without an explicit decision (GPL); Dolphin is a behavioural reference only.
- CI (Gitea Actions): Linux, Windows, macOS; build and instruction tests, no ISO.
