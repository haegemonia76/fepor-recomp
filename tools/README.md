# Tools

Reusable helpers. Extend these (or the `fepor-recomp` subcommands) instead of writing one-off scripts.

| Tool | Purpose |
| --- | --- |
| `fepor-recomp extract` | Extract `sys/` and `files/` from the disc image into `--data`. |
| `fepor-recomp analyze` | Function starts, jump tables and warnings; report in `<data>/analysis.txt`. |
| `fepor-recomp generate` | Generate the C++ project into `<data>/generated`. |
| `fepor-recomp prepare` | Generate and build; the RecompLauncher setup step (`progress` lines, stable exit codes). |
| `fepor-recomp disasm --addr 0x80003154 --count 16` | Disassemble guest code from main.dol. |
| `fepor-recomp opcodes` | Instruction histogram of main.dol (what the lifter must cover). |
| `fepor-recomp find --op Rfi` | Every occurrence of an opcode with its function (locating SDK internals). |
| `fepor-recomp find --pointers 0x8026b2a8` | Data words and `lis`+`addi/ori` pairs that reference an address (function pointers). |
| `fepor-recomp card-info <image.raw>` | Memory card image check: header/directory/BAT checksums and the saves on the card. |
| `fepor-recomp card-import <card.raw> <save.gci>` / `card-export <card.raw> <out.gci> [--name N]` | Move saves between a raw card image and Dolphin's `.gci` files (a save exported from a card is byte-identical to the one Dolphin wrote). |
| `tools/dolphin-run.sh --iso <disc> [--card <raw>] -- wait:12 press:A mash:START:40 shot:out.png ...` | Run the game in Dolphin (reference emulator) in an isolated user dir, driven through its pipe controller, with screenshots: compare saves, memory cards and frames with the recomp. Needs `dolphin-emu` and KDE's `spectacle`. |
| `fepor-recomp ls` | Disc files with offsets and sizes (match `FEPOR_TRACE=di` reads, which print offset/4). |
| `tools/dev-run.sh <disc>` | Development loop: generate (with call tracing), build, link with the Rust runtime (`crates/game`), run. Env: `FEPOR_TRACE=hw,calls`, `FEPOR_HW_LIMIT=n`, `FEPOR_WATCHDOG=secs`, `SKIP_PREPARE=1`. Video/input env: `FEPOR_HEADLESS=1` (no window; required for dumps in CI), `FEPOR_DUMP_AUDIO=file.wav`, `FEPOR_NO_AUDIO=1`, `FEPOR_MEMCARD_A=<file|none>`, `FEPOR_DUMP_FRAMES=dir`, `FEPOR_DUMP_EVERY=n`, `FEPOR_DUMP_TEXTURES=dir`, `FEPOR_PAD=A@400+8,...`, `FEPOR_NO_VIDEO=1`, `FEPOR_TRACE=gx,gxdraw,dsp,di,irq,vi`, `FEPOR_WATCH=<hex addrs>` (log arguments on entry), `FEPOR_RECENT=1` (long call history in crash reports). |
| `tools/vectors.sh [out]` | Regenerate `crates/ppc-lift/tests/vectors.bin` with Unicorn (`gen_vectors.py`, ADR 0008). Output is deterministic. |
| `tools/validate-profile.sh [file]` | Validate `packaging/recomplauncher.json` against RecompLauncher's schema. |
| `tools/venv.sh` | Sourced by the scripts above: creates `.venv-tools` from `tools/requirements.txt`. |

All `fepor-recomp` commands take `--disc/--game`, `--output/--data` (or `RECOMPLAUNCHER_DATA`), `--config` and `--no-verify`.
