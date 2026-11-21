# 0002 Toolchain and repo structure

Status: accepted

- Rust workspace: `core`, `ppc-lift`, `cli`, `gui` (egui). CLI and GUI share the core library and one TOML config.
- Decoding via `ppc750cl` (licence and Gekko coverage to verify first); the lifter is ours.
- Runtime in C++ built with cmake, ninja and clang on all three OS, x64 and arm64.
- Symbols: read-only reference to the community `fe9` decomp after a licence check.
- Licence: GPL-2.0-or-later (decided during M1.3 so Dolphin's FP estimate tables and algorithms can be reused; initially planned as MIT). Files derived from Dolphin carry an SPDX header and attribution. Dolphin remains the behavioural reference.
- CI (Gitea Actions): Linux, Windows, macOS; build and instruction tests, no ISO.
