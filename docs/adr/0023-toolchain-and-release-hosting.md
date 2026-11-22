# 0023 Toolchain for players and where releases live

Status: accepted (decides ticket M7.3; ADR 0010 and 0011)

**1. Toolchain: none for players, and no stopgap.**
- The target is ADR 0011: one executable, `prepare` generates native code in-process (ticket M7.0, Cranelift), nothing to install.
- Until it exists the only working path is the development one (`tools/dev-run.sh`): clang, CMake, Ninja **and** the Rust toolchain, because the game binary links the code generated from the player's disc and cannot be shipped prebuilt. Asking players to install that, downloading a pinned toolchain on first run, or bundling clang, CMake, Ninja and Rust (hundreds of MB per platform) would all be work thrown away when M7.0 lands, and the first two make the player's setup depend on our servers or on a compiler installation.
- So: **nothing is published for players before M7.0**. The profile stays in `packaging/`; the development path is for people who build from source (they already have the tools). Exit code 20 stays in the profile for the development backend and disappears with it.
- A cheaper route to a shippable release than M7.0 exists and is recorded here, not chosen: `prepare` builds the generated code as a shared library that a prebuilt `fepor-game` loads (needs the runtime's `fepor_*` symbols exported by the executable on each platform, and a C++ toolchain at setup, so it only removes the Rust requirement). Not worth it unless M7.0 turns out much later.

**2. Releases: a GitHub mirror, built there.**
- RecompLauncher installs from GitHub releases ("Nothing goes through us"; its guide), and takes the build's files from the release assets. For a project on a Forgejo/Gitea server it only reads `recomplauncher.json` from the default branch of the repository, not the builds. A catalog entry cannot host the archives either.
- Decision: keep Gitea as the home of the project (code, tickets, review) and add a **GitHub repository as a push mirror** of `main` and the tags. The release workflow (`.github/workflows/release.yml`, written when M7.0 exists) runs on a `v*` tag there and attaches `fepor-recomp-<version>-<windows|linux|macos>-<x64|arm64>.<zip|tar.xz>` with `recomplauncher.json` at the top and no game data (ADR 0010). Tickets and pull requests stay on Gitea; the mirror says so in its README.
- RecompLauncher also reads the profile from the Gitea default branch when a release has none: the profile moves to the repository root only when a release works end to end, as ADR 0010 says.
- Needs from the owner, when M7.0 is close: a GitHub account or organisation and an empty repository to mirror to, and the Gitea push-mirror credentials. Nothing is created before that.

**Consequences.** Ticket M7.1 (release workflow, end-to-end install) waits for M7.0. Ticket M7.0 is the next piece of work that unblocks player releases.

## Update: the release workflow (ticket M7.1, after M7.0)

M7.0 exists (ADR 0024), so `.github/workflows/release.yml` is written. A `v*` tag pushed to the GitHub mirror builds on four runners, packages with `tools/package-release.sh` and attaches the archives with `gh release create`.

- **Version.** The tag must equal `[workspace.package] version` (the job fails otherwise); the profile inside each archive gets `prepare.version` set to it, which is what RecompLauncher compares to decide that setup must run again.
- **Archive.** `fepor-recomp-<version>-<system>-<arch>.<zip|tar.xz>` (zip on Windows) with, at the top: `fepor-recomp[.exe]`, `fepor-game[.exe]`, `recomplauncher.json`, `LICENSE`. No game data. Nothing else is needed on the player's machine for the executables: SDL3 is compiled in (`--features fepor-game/bundled-sdl`, `sdl3/build-from-source-static`), so the build needs cmake and a C compiler, and on Linux the SDL backend headers (listed in the workflow).
- **Systems built: Linux x64, Windows x64, macOS arm64 and macOS x64.** Linux and Windows on arm64 are not built: nobody on the project can run them, and shipping an executable nobody has started would only look supported. The code is portable (Cranelift targets the host, the calling convention is the host's), so adding them later is two lines in the matrix once someone can test.
- **Not yet checked.** The workflow has not run (it needs the mirror), and no archive was started on Windows or macOS. The Linux x64 build with bundled SDL3 and the packaging script were run on the development machine.
- `sdl3-src` is pinned to 3.2.30 in `Cargo.lock`: `sdl3-sys` accepts any 3.x and 3.4 ships without the `test` directory its CMake build asks for.
- The profile stays in `packaging/`: it moves to the repository root when a release has been installed end to end (ticket acceptance).

**Setting up the mirror (owner).** A GitHub repository, empty; on Gitea, Settings, Repository, Mirror settings, push mirror to it with a token that can push `main` and tags. Release: bump the version, push the commit, then `git tag v<version> && git push origin v<version>`.
