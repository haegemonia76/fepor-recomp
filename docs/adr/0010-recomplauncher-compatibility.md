# 0010 RecompLauncher compatibility

Status: accepted

Players should be able to install, set up and play this recomp from [RecompLauncher](https://recomplauncher.com) (profile format: its `docs/DEVELOPERS.md`, schema validated by `tools/validate-profile.sh`). Constraints this puts on the project:

- **Profile**: `packaging/recomplauncher.json` ships at the top of every release archive, next to `fepor-recomp`. Moved to the repository root only when a release works end to end (the launcher reads the default branch of Forgejo/Gitea repos).
- **One data folder**: everything the tool or the game writes goes under `--data <dir>` (alias of `--output`, default from `RECOMPLAUNCHER_DATA`): `generated/`, `build/`, `cache/`, `logs/`, `saves/`, `config.toml`. Nothing is written elsewhere. `--game <path>` is an alias of `--disc`.
- **`prepare`**: `fepor-recomp prepare --game {game} --data {data}` is the setup step. It never prompts and prints `progress <0..1> <text>` lines on stdout, logs on stderr, is incremental (unchanged generated files are not rewritten, so ninja rebuilds nothing), and writes `{data}/prepared.json` last (the profile's `done`). Exit codes are stable: 10 disc unreadable, 11 unsupported game/version, 20 toolchain missing (clang, cmake, ninja), 30 build failed; mapped to messages in `exit_codes`.
- **`start`**: `fepor-recomp run --data {data}` launches `fepor-game` (next to it, else on the PATH), without `{game}` so that the game plays without the player's copy. The game reads `RECOMPLAUNCHER_MODE` (`bigpicture` starts full screen for that run) and `RECOMPLAUNCHER_PAD` (`xbox` or `playstation`: the button names of a new `input.toml`); `run` keeps everything the game prints in `{data}/logs/fepor-game.log` (the previous four runs as `.1.log` to `.4.log`) and exits with the game's code; nothing updates itself; the game quits on the overlay's Quit page, and is playable on a controller from start to finish.
- **Saves and settings**: saves are the Dolphin-compatible memory card image in `{data}/memcards` (the profile's `saves`); user options live in `{data}/settings.toml` and `{data}/input.toml`, and the profile's `config` and `settings` write them: text language, resolution, frame multiplier, picture shape, window mode, V-Sync, statistics (advanced), volume, button names. Remapping stays in the in-game overlay. Values the launcher forces for one run (`RECOMPLAUNCHER_MODE`, the `FEPOR_*` overrides) are not written back to `settings.toml`; only what the player changes is.
- **Disc formats**: the launcher's GameCube standard is ISO, RVZ and GCZ; the disc reader reads all three (ADR 0022).
- **Releases**: one archive per system and architecture named `fepor-recomp-<version>-<windows|linux|macos>-<x64|arm64>.<zip|tar.xz>`; no game data in any archive.
- **Toolchain**: decided in ADR 0011: one executable, no tools for players (the C++ `prepare` described above is the development backend).
- **Releases host**: decided in ADR 0023: a GitHub push mirror of this repository builds and publishes the archives; nothing is published before the single executable exists.
- **Not done yet (ticket M7.1)**: the release workflow and the end-to-end install. A release cannot hold a working `fepor-game`: the game links the code generated from the player's disc, so it only exists after `prepare`, on a machine with clang, CMake, Ninja and the Rust toolchain. It waits for the single executable (ADR 0011, ticket M7.0) or for loading the generated code as a library built by `prepare`; the profile stays in `packaging/` until then.

## Tooling rule

Scripts and tools are reusable and live in `tools/` (documented in `tools/README.md`) or as `fepor-recomp` subcommands. Extend an existing tool before adding one; no throwaway scripts in the repo or in commit history.
