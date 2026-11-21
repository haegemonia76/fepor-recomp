## Agent skills

### Issue tracker

Issues live in Gitea Issues (`tea` CLI). See `docs/agents/issue-tracker.md`.

### Triage labels

Default five-label vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

## Tooling

Scripts and tools must be reusable: put them in `tools/` (see `tools/README.md`) or add a `fepor-recomp` subcommand. Extend an existing tool before creating a new one; do not leave throwaway scripts in the repo.

Testing against the reference emulator is a tool too: use `tools/dolphin-run.sh` (and extend it) instead of ad-hoc commands, so every Dolphin comparison can be repeated.

### RecompLauncher

The recomp must stay compatible with RecompLauncher: see `docs/adr/0010-recomplauncher-compatibility.md` (one data folder via `--data`, `prepare`/`start` contract, stable exit codes, profile in `packaging/`).
