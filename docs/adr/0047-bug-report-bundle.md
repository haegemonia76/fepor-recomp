# 0047 Bug report bundle

Status: accepted (implements ticket #47)

**What**
- `fepor-recomp bug-report [--out file.zip]`, the Help page of the overlay ("Make bug report") and the launcher GUI's "Bug report" button write one zip to `<data>/bug-reports/fepor-bug-report-<unix time>.zip` and show its path. All three call `fepor_core::bugreport`.
- Contents: `report.txt` (build, engine, game ID, GPU adapter, graphics API, OS, `FEPOR_TRACE` of the last run, file list), `settings.toml`, `input.toml`, `mods/mods.toml` and `cheats.toml` (the enabled mods and cheats by name; no mod files, no cheat definitions), the names and sizes of the card backups in `memcards/backups` (listed in `report.txt`; the images are never read), every `*.log`/`*.txt` of `<data>/logs` (the trace of `FEPOR_TRACE` is printed to the log, so it is in there), `logs/session.toml` and the newest `.png` of `<data>/screenshots` as `screenshot.png` when that folder has one.
- Never in it: memory cards, imported saves, the game data (`disc/`), generated code. `is_excluded` rejects those folders and file types (`.raw .gci .iso .rvz .gcz .dol .bin ...`) for every entry, whatever collected it.
- The home directory (`HOME`, `USERPROFILE`, and its resolved form) becomes `~` in all text, with `/`, `\` and TOML-escaped `\\` separators; `/home/bobby` is not touched for `/home/bob`.

**How**
- Only the running game knows the build, engine, game ID and GPU: it writes `<data>/logs/session.toml` at start (`update_session`) and the renderer adds the adapter. The CLI and GUI read that record, so a report made while the game is not running is complete.

**Limits**
- Nothing creates `<data>/screenshots` yet; the zip takes a screenshot only if one is there.
- Only the home directory is stripped; other personal text in a log (a user name outside a path) is not detected.
