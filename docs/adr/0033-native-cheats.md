# 0033 Native cheats (TOML)

Status: accepted (ticket #33; builds on ADR 0025 patch engine, ADR 0032 cheat engine, ADR 0048 memory watcher)

**What**: named cheats of one game ID written as TOML, listed on the overlay's Cheats page in a "Native cheats" section above the Gecko / Action Replay codes, turned on and off the same way, run by the same `CheatRunner` and patch engine (sources `cheat:<name>`, continuous sets).

**Files**
- Shipped: `crates/core/cheats/<game id>.toml`, compiled in (`fepor_core::native`). Addresses, sizes and values only; no game data (ADR 0003).
- Player's own: `<data>/cheats/<game id>.toml`; a cheat of the same name replaces a shipped one. A file that is not TOML is reported on the page; a single cheat that cannot be read (typo, address outside RAM, value too large) is listed as unusable with the reason.
- Enabled state: the same `<data>/cheats.toml` as the Gecko codes, keys `native:<name>`. A headless run can enable cheats through this file (that is how each shipped cheat was checked).

**Schema** (the snippet of `memwatch::cheat_toml`, ADR 0048, is valid as is):
```toml
[[cheat]]
name = "..."            # unique key
description = "..."     # optional
game = "GFEP01"         # optional: skipped for another game
continuous = true       # held every frame; absent/false: written once when turned on
[[cheat.write]]
address = 0x8033A720    # cached, uncached or physical main RAM address
size = 4                # 1, 2 or 4 bytes, big-endian (default 4)
value = 9999999
[cheat.write.when]      # optional condition
address = 0x802BBB50
size = 4                # default 4
op = "eq"               # eq (default) ne lt le gt ge, unsigned
value = 0x80900E10
```

**Condition**: the one engine change. `Patch` has an optional `when` (`cheats::Condition`), checked by the patch engine at each write (when enabled, and every frame for a continuous set) on the RAM of that moment. It is not a program: one comparison. Used to guard a heap address ("write only while the record is where I found it").

**Debug aid**: `FEPOR_DUMP_RAM=<dir>[:every[:from[:to]]]` writes main RAM from the frame hook (tools/README.md) for offline searches.

## Verified cheats (GFEP01, 1.00 EU, `--game` extracted disc, headless)

| Cheat | Address | How it was found / verified |
| --- | --- | --- |
| Max funds | `0x8033A720` u32 | Field `+0x18` of the global struct at `0x8033A708`; the game's debug code prints it with `"Funds :%7dG"`; initial value 5000 in the prologue; the cap 9,999,999 is that of the decrypted public PAL Action Replay code (decoded with Dolphin's own AR decryption). Run with the cheat on: RAM dump at frame 10020 reads 9999999 (the game never rewrote it). No funds screen exists in the reachable prologue, so there is no frame proof. |
| Max bonus EXP | `0x8033A724` u32 | Field `+0x1C`, `"EvtPts:%7dP"`; same run reads 99999. Same limit (no screen). |
| Ike HP 99 (prologue) | `0x802BBB6C` u8, guarded by `0x802BBB50 == 0x80900E10` | Found by diffing the unit objects (stride `0x280`, `+0x1A8`) of Ike (19) and Boyd (18) in RAM dumps. Writing 7 made the unit hover window read `HP 7/19`; with the shipped cheat it reads 99. |

## Research notes (not shipped: not verified)
- **Gold/EXP screens**: none reachable without a save (the run reaches the prologue map with a scripted new game). Funds and bonus EXP are verified by memory re-read only.
- **Item uses**: item slot is 8 bytes (id word, uses byte at `+4`, flags `+5`; from the copy routine at `0x8001C604`, the public "infinite usage" code patches `lbz r0,4(r4)` there, which a recompiled game cannot take). The address of Ike's slot in the unit object was not found.
- **Unlimited movement / 100% hit**: Mov and Hit are derived. The info-window copy at `0x8033B144` (u16: Atk, Def, Res, Hit, Avo, ..., Dodge) shows Ike's Hit 173 but is a computed copy; the stored stat bytes were not identified (writing into the `0x802BBB50` record at guessed offsets crashed or blanked the game). Prologue enemies cannot be fought within the scripted runs (Boyd waits until Ike walks to him).
- **Unlock everything** (public decrypted code): `0x8033BFC9` = `0x3F` (trial maps), 15 bytes `1..15` from `0x8033BFCA` (bonus characters). Not shipped: no screen shows them without a save/title menu check.
- Debug display: byte `0x803759C8` = 1 draws the engine's process list (seen working through this cheat path).

## Limits
Heap addresses (Ike's HP) are valid for the prologue map layout seen; the condition keeps a wrong layout from being written but the cheat then does nothing. One comparison per write, no pointer chains.
