# fepor-recomp

Static recompilation of *Fire Emblem: Path of Radiance* (GameCube, European release `GFEP01`) into a native desktop program. You supply your own disc image; this project contains no Nintendo content.

**This GitHub repository is a read-only mirror.** Development, issues and review happen on [gitea.gerard-le-cloerec.fr/haegemonia76/fepor-recomp](https://gitea.gerard-le-cloerec.fr/haegemonia76/fepor-recomp); `main` and the tags are pushed here, and a `v*` tag builds the release archives that [RecompLauncher](https://recomplauncher.com) installs from.

## Playing

Install the release in RecompLauncher (it uses `recomplauncher.json` from the archive), point it at your disc image (`.iso`, `.rvz`, `.gcz`, or an extracted folder), and let it run the setup. Setup analyses the game and copies its files next to your saves; it takes a few seconds and needs no compiler.

Without the launcher: `fepor-recomp prepare --game <disc> --data <folder>`, then `fepor-recomp run --data <folder>`.

**arm64 is not tested.** The macOS arm64 archive is built by GitHub, but nobody on the project has arm64 hardware (any OS) to run it: it is published as is, untested. Linux and Windows arm64 are not built at all for the same reason (see ticket 28). Linux x64 is the only archive that has been run (headless, from the downloaded release).

## Building from source

```
cargo build --release -p fepor-recomp -p fepor-game
```

Design notes are in [`docs/adr`](docs/adr), the vocabulary in [`CONTEXT.md`](CONTEXT.md), the helper scripts in [`tools`](tools/README.md). Licence: GPL-2.0, see [`LICENSE`](LICENSE).
