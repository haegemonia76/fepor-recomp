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
| `tools/dev-run.sh <disc>` | Development loop: generate (with call tracing), build, link with the Rust runtime (`crates/game`), run. Env: `FEPOR_TRACE=hw,calls`, `FEPOR_HW_LIMIT=n`, `FEPOR_WATCHDOG=secs`, `SKIP_PREPARE=1`. |
| `tools/vectors.sh [out]` | Regenerate `crates/ppc-lift/tests/vectors.bin` with Unicorn (`gen_vectors.py`, ADR 0008). Output is deterministic. |
| `tools/validate-profile.sh [file]` | Validate `packaging/recomplauncher.json` against RecompLauncher's schema. |
| `tools/venv.sh` | Sourced by the scripts above: creates `.venv-tools` from `tools/requirements.txt`. |

All `fepor-recomp` commands take `--disc/--game`, `--output/--data` (or `RECOMPLAUNCHER_DATA`), `--config` and `--no-verify`.
