# 0032 Cheat codes (Gecko and Action Replay)

Status: accepted (ticket #32; builds on the patch engine of ADR 0025 and the overlay of ADR 0020)

**What**: Dolphin game INI files (`GFEP01.ini`) can be imported; their Gecko and Action Replay codes are listed on the overlay's **Cheats** page, turned on and off one by one, and the choice is kept.

**Format** (`fepor_core::cheats`): `[Gecko]` and `[ActionReplay]` with `$Name [Author]` headers (`+Name` is read too), `*` note lines and `XXXXXXXX XXXXXXXX` lines; `[Gecko_Enabled]` / `[ActionReplay_Enabled]` list the codes that are on. Other sections (`[Core]`, `[OnFrame]`, ...) are skipped. A line that is not hex makes its code unusable (shown, not runnable), the file still loads.

**Supported code types** (plain data writes only, because the patch engine rewrites fixed bytes):
- Gecko: `00` write 8-bit (repeated `ZZZZZZ+1` times), `02` write 16-bit (repeated `ZZZZ+1` times), `04` write 32-bit, `06` string write. Address `0x80000000 | (word & 0x01FFFFFF)`.
- Action Replay: normal RAM write and fill, 8, 16 and 32 bits (same address form).

**Not supported** (kept in the list, checkbox disabled, reason shown): Gecko conditionals (`20`-`2F`, `C0`...), loops and jumps (`66`, `68`...), base address / pointer addressing (`ba`/`po`, `80`-`A6`, bit `0x10` of the address word), serial write (`08`), assembly insertion (`C0`, `C2`, `C3`, `D2`...), `E0`/`E2` and everything else; Action Replay conditionals, pointer writes, add codes, master codes and fill-and-slide. Conditionals and loops would need a program run against RAM each frame; the patch engine only holds fixed bytes and the hook has no code to run, so nothing is faked. Also not runnable: a code that writes into the DOL's text sections (the code is recompiled, ADR 0025: it would change nothing).

**How**
- `CheatStore` (core) is shared by the overlay (render thread: list, toggle, import) and the guest thread through `Shared::cheats`; it has a generation counter. `fepor_runtime::cheats::CheatRunner` (`machine().cheats`) is synced from `intr::poll` just before the patch engine's frame hook: when the generation moved it unregisters sets of codes turned off or changed (reverting the bytes) and registers + enables the sets of codes turned on. Each code is one `PatchSet` with source `cheat:<name>` and `continuous: true`: Gecko / AR codes run every frame in Dolphin, so the game's own writes are overwritten at the next frame; turning the code off puts back the bytes of the moment it was turned on. No second hook.
- A code the engine refuses (overlap with an enabled set of another source, e.g. a mod) stays off and the Cheats page shows the reason in red next to it.
- Storage in the data folder, which is the profile (it also holds `settings.toml` and the memory cards): the imported INI is copied to `<data>/cheats/<game id>.ini` and read at every start; the enabled codes are `[enabled]` in `<data>/cheats.toml` keyed by game ID (`GFEP01 = ["gecko:Infinite Money"]`). The first time (no entry for the game) the INI's own `_Enabled` lists decide; importing again keeps the codes that were on and adds those the file lists as enabled.
- Import uses the same `rfd` chooser thread pattern as the memory card page. Only the player's own INI is read; no codes ship with the repository (ADR 0003).

**Limits**: data writes only (see ADR 0025: behaviour comes from data the code reads); a code whose address is wrong for the running game is not detected, only the main-RAM range is checked; one imported INI per game ID (a new import replaces the list); two codes writing the same bytes conflict (different sources) and the second stays off.
