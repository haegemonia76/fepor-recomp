# 0010 RecompLauncher compatibility

Status: accepted

Players should be able to install, set up and play this recomp from [RecompLauncher](https://recomplauncher.com) (profile format: its `docs/DEVELOPERS.md`, schema validated by `tools/validate-profile.sh`). Constraints this puts on the project:

- **Profile**: `packaging/recomplauncher.json` ships at the top of every release archive, next to `fepor-recomp`. Moved to the repository root only when a release works end to end (the launcher reads the default branch of Forgejo/Gitea repos).
- **One data folder**: everything the tool or the game writes goes under `--data <dir>` (alias of `--output`, default from `RECOMPLAUNCHER_DATA`): `generated/`, `build/`, `cache/`, `logs/`, `saves/`, `config.toml`. Nothing is written elsewhere. `--game <path>` is an alias of `--disc`.
- **`prepare`**: `fepor-recomp prepare --game {game} --data {data}` is the setup step. It never prompts and prints `progress <0..1> <text>` lines on stdout, logs on stderr, is incremental (unchanged generated files are not rewritten, so ninja rebuilds nothing), and writes `{data}/prepared.json` last (the profile's `done`). Exit codes are stable: 10 disc unreadable, 11 unsupported game/version, 20 toolchain missing (clang, cmake, ninja), 30 build failed; mapped to messages in `exit_codes`.
- **`start`**: `fepor-recomp run --game {game} --data {data}` (M2) launches the built game. It reads `RECOMPLAUNCHER_MODE` (`bigpicture` starts full screen), `RECOMPLAUNCHER_PAD` (default glyph set), never updates itself, quits cleanly, writes logs to `{data}/logs`, is playable on a controller from start to finish.
- **Saves and settings**: saves are the Dolphin-compatible memory card image in `{data}/saves`; user options live in the shared TOML config so the profile's `config`/`settings` can expose them (resolution, frame rate, glyph set, remapping) as M5/M6 add them.
- **Disc formats**: the launcher's GameCube standard is ISO, RVZ and GCZ; the disc reader must gain RVZ and GCZ (ticket).
- **Releases**: one archive per system and architecture named `fepor-recomp-<version>-<windows|linux|macos>-<x64|arm64>.<zip|tar.xz>`; no game data in any archive.
- **Open decisions**: the launcher installs from GitHub releases (a Gitea-only repository needs a mirror or a catalog entry); the player needs a clang/CMake/Ninja toolchain unless we bundle or download one.

## Tooling rule

Scripts and tools are reusable and live in `tools/` (documented in `tools/README.md`) or as `fepor-recomp` subcommands. Extend an existing tool before adding one; no throwaway scripts in the repo or in commit history.
