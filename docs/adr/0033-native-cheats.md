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
kind = "hold"           # hold (default) | add | set, see "Kinds"
continuous = true       # hold only: held every frame; absent/false: written once when turned on
min = 0                 # add / set only: bounds of the result (default 0 and the largest value of `size`)
max = 9999999
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

**Kinds** (ticket #65)
- `hold` (default): the cheat writes its `value` when turned on (checkbox), every frame if `continuous`. Turning it off puts back the bytes the patch engine saved when it was turned on (ADR 0025): gold earned meanwhile is lost, so it is the wrong kind for funds.
- `add`: no checkbox, an input and an Apply button. One `[[cheat.write]]` without `value` (address, size, optional `when`). Apply reads the current big-endian value once, adds the typed amount (negative subtracts), clamps to `min..=max`, writes it. Nothing is held and nothing is ever reverted; the result of the last Apply (`5000 -> 6234`, "limited to ..") is shown under the cheat.
- `set`: same, writes the typed value clamped to `min..=max`.
An `add`/`set` cheat refuses `value`, `continuous` and a second write; `hold` refuses `min`/`max`. Apply is done by the guest thread (`CheatStore::request` → `CheatRunner::sync` → `Cheat::apply`), directly on RAM, not through the patch engine: there is no patch set to revert. If its `when` condition does not hold, nothing is written and the page says so. Headless: `FEPOR_MEMSCRIPT_FILE` step `apply AMOUNT native:<name>` (tools/README.md).

**Condition**: the one engine change. `Patch` has an optional `when` (`cheats::Condition`), checked by the patch engine at each write (when enabled, and every frame for a continuous set) on the RAM of that moment. It is not a program: one comparison. Used to guard a heap address ("write only while the record is where I found it").

**Debug aid**: `FEPOR_DUMP_RAM=<dir>[:every[:from[:to]]]` writes main RAM from the frame hook (tools/README.md) for offline searches.

## Verified cheats (GFEP01, 1.00 EU, `--game` extracted disc, headless)

| Cheat | Address | How it was found / verified |
| --- | --- | --- |
| Add funds / Set funds (add, set) | `0x8033A720` u32, 0..9,999,999 | Field `+0x18` of the global struct at `0x8033A708` (debug text `"Funds :%7dG"`); cap from the public PAL Action Replay code. **Verified on the game's own screen** (map, START status screen "Prologue: Mercenaries / Turns / Funds"): baseline 5000 G; Apply Add 1234 → 6234 G; Add 3000 while the screen is open → 9234 G (the screen reads the word live); the "game earns/changes gold" case (a poke to 7777) stays 7777 (nothing restores the old value); Add 5,000,000,000 → 9999999 G (clamped); Set 2500 → 2500 G. An earlier version of this ADR claimed the word from a RAM re-read only: a first poke test showed the game does not rewrite it, and the screen follows it. |
| Add bonus EXP / Set bonus EXP | `0x8033A724` u32, cap 99,999 | Field `+0x1C` (`"EvtPts:%7dP"`). The adjacent funds word is confirmed to be the displayed one, and the Apply path is the same, but **no screen showing bonus EXP is reachable in the prologue map** (it is in the preparation / save screens), so that this word is the bonus EXP the player sees is **not confirmed**. The cap 99,999 is the public AR code's, also unconfirmed on screen. |
| Ike HP 99 (prologue) | `0x802BBB6C` u8, guarded by `0x802BBB50 == 0x80900E10` | Found by diffing the unit objects (stride `0x280`, `+0x1A8`) of Ike (19) and Boyd (18) in RAM dumps. Writing 7 made the unit hover window read `HP 7/19`; with the shipped cheat it reads 99. |

### Ike's stats (ticket #66, prologue map, headless)
| Cheat (set) | Address | Verified |
| --- | --- | --- |
| Set Ike current HP | `0x802BBB6C` u8 | the map hover window and the status screen read `HP 7 / 25` after Apply 7 |
| Set Ike base max HP / Str / Mag / Skl / Spd / Def / Res | `0x80900E54..5B` (HP, Str, Mag, Skl, Spd, **not 0x59**, Def, Res) u8 | Apply 24 / 10 / 9 / 12 / 11 / 8 / 4: the status screen (Y on the map) reads max HP 25, Str 12, Mag 9, Skill 15, Sp 14, Def 8, Res 4. |

All guarded by `0x802BBB50 == 0x80900E10`. What was found:
- Unit objects: array from `0x802BB9C4`, stride `0x280` (Ike, Boyd, ...; empty slots are zero), table of pointers to them at `0x802D4744`. Object `+0x18C` is a pointer to the unit's record (heap), `+0x1A8` the current HP.
- The record (`0x80900E10`) holds, at `+0x44..+0x4B`, the **base** stats; the screen shows base + a class bonus (Ike: HP +1, Str +2, Mag 0, Skl +3, Spd +3, Def 0, Res 0) clamped to the class caps (bytes `+0x4C..` hold caps 40/20/15/20/20/../20/20: writing 33 to Str displayed 20). That is why the cheats are named *base*; the displayed value is base + bonus, not the typed value.
- The sixth byte (`0x80900E59`, where Luck would be) is **not** Luck: writing it changed nothing. Luck (6), level, EXP, Mv and Wt were not found. Writing guessed bytes of the unit object (`+0x198..0x1CB`, all at once) changed the course of the game; writing `0x80900E50/51` and `+0x198` blanked the screen. Nothing of that is shipped.
- **Which unit the status screen shows**: not found. The prologue map has one player unit; "Next Unit" (R) stays on Ike, so no run could tell a global pointer/index from Ike's slot. Hence the stat cheats are for **Ike only** (the unit whose record the guard names), not "the displayed unit". Finding the link needs a map with several player units (not reachable headless without the first chapters).

## Research notes (not shipped: not verified)
- **Bonus EXP screen**: see the table above (the funds screen was reached with the maintainer's first save, copy of the card, `FEPOR_MEMCARD_A`).
- **Item uses**: item slot is 8 bytes (id word, uses byte at `+4`, flags `+5`; from the copy routine at `0x8001C604`, the public "infinite usage" code patches `lbz r0,4(r4)` there, which a recompiled game cannot take). The address of Ike's slot in the unit object was not found.
- **Unlimited movement / 100% hit**: Mov and Hit are derived. The info-window copy at `0x8033B144` (u16: Atk, Def, Res, Hit, Avo, ..., Dodge) shows Ike's Hit 173 but is a computed copy; the stored stat bytes were not identified (writing into the `0x802BBB50` record at guessed offsets crashed or blanked the game). Prologue enemies cannot be fought within the scripted runs (Boyd waits until Ike walks to him).
- **Unlock everything** (public decrypted code): `0x8033BFC9` = `0x3F` (trial maps), 15 bytes `1..15` from `0x8033BFCA` (bonus characters). Not shipped: no screen shows them without a save/title menu check.
- Debug display: byte `0x803759C8` = 1 draws the engine's process list (seen working through this cheat path).

## Limits
Heap addresses (Ike's HP) are valid for the prologue map layout seen; the condition keeps a wrong layout from being written but the cheat then does nothing. One comparison per write, no pointer chains.
