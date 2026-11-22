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
