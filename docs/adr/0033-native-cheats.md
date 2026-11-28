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

## Character stats (ticket #66)

**What the page shows**: a *Character* section for the unit the game displays, one row per stat: `Base | Class | Other | Total | New base`. `Total` is what the status sheet shows; `Base` is the only value the cheat writes. **One semantic**: the input edits the *Base*, the row previews `-> new total = min(typed + Class + Other, cap)`, the range of the input (`min-max`) is shown on the row and an out-of-range input is refused inline (Apply disabled, red); the engine clamps again on Apply. `Class` and `Other` are read live from memory at every refresh, never hardcoded.

**Schema** (`[character]` in a cheat TOML, one table per game; the player's file replaces the shipped one): `unit` (global word holding the address of the displayed unit object), `person_at` / `job_at` (offsets in the unit object of the person and job record pointers), optional `person_check` / `job_check` (globals that hold the same two pointers; the unit is refused when they disagree), and `[[character.row]]` with `label`, `base` (`{rec = "unit"|"person"|"job", off = N}`, one signed byte), optional `class`, `other`, `cap` (same form; a cap byte of 0 means none stored), `min`, `max_total` (highest total when no cap is stored), `max_total_of` (another, earlier row: current HP never exceeds max HP) and `note`. Code: `fepor_core::character`.

**How the displayed unit is found** (Ch. 1 save of the maintainer's card, four player units, headless; `UP` / `DOWN` on the status sheet are "Next Unit"/previous, `RIGHT`/`LEFT` the pages, `Y` on the map opens it): RAM dumps with Ike and with Titania on the sheet, then the words that are equal across two dumps of the same unit and different between the units: `0x8033B12C` = the unit object (`0x802BB9C4` Ike, `0x802BC144` Titania), `0x8033B034` / `0x8033B038` = its person / job record pointers (the same global struct as the info window copy at `0x8033B144`). It follows the unit the game last displayed (sheet or map hover window); there is no cheap "a sheet is open" flag, so the page says "Open a character sheet (Y on the map)" only while no valid unit is there (before any unit was shown, or the pointers disagree / the records are not `PID_*` / `JID_*`). The name is read from the person record's id string (`PID_IKE` -> Ike), the class from the job record's (`JID_RANGER`).

**Layout** (identical in the prologue and Ch. 1; unit objects from `0x802BB9C4`, stride `0x280`):
| Part | Where | Notes |
| --- | --- | --- |
| person record (`unit+0x188`, heap, one per character) | `+0x00` pointer to `PID_*`; `+0x1D..0x24` HP Str Mag Skl Spd Lck Def Res | the character's own bonus: **Base**, what the cheat writes |
| job record (`unit+0x18C`, heap, **shared by every unit of the class**) | `+0x00` pointer to `JID_*`; `+0x3D` Wt, `+0x3E` Mv; `+0x44..0x4B` class base HP..Res; `+0x4C..0x53` caps (HP 40, Str 20, Mag 15, ...; **Lck 0 = none**) | **Class**; never written (a write would change all units of the class; this is what the first version of this ticket did by mistake: it wrote the job record and called it "base") |
| unit object | `+0x19A` level, `+0x19B` EXP, `+0x1A8` current HP, `+0x1AB..0x1B2` HP..Res **gains** (level-ups, Ike Lv 3: HP +2, Spd +2, Lck +1, Def +1, Res +2) | **Other** |

Sheet = `min(person + job + gains, cap)`: checked on the sheet for Ike (prologue: 19/5/1/6/7/6/5/0 = person 1/2/0/3/3/6/0/0 + job 18/3/1/3/4/0/5/0; Ch. 1 Lv 3: 21/5/1/6/9/7/6/2 with the gains above) and Titania (Lv 1 Paladin: 33/12/4/13/14/11/11/7 = person 10/5/2/9/7/11/3/4 + job 23/7/2/4/7/0/8/3, no gains). **Other** therefore is the unit's level-up gains; weapon, support, status or item bonuses do not enter these sheet values (not seen, not modelled).

**Ranges**: Base `0..(cap - Class - Other)` (higher would only be cut by the cap; stats), cap = the job record's cap byte; luck stores no cap: `max_total = 40` (tested: Lck 39 on Ike shows 40). Lv 1-20 (the stats are **not** recomputed), EXP 0-99, current HP 1..max HP. Bytes are signed; nothing outside `-128..127` can be written.

| Row | Verified (headless, Ch. 1 save, frame dumps `shots/68-*.png`) |
| --- | --- |
| Ike Str, Mag, Lck, HP now | Apply 10 / 8 / 39 / 5: the sheet reads Str 13 (10 + class 3 + other 0), Mag 9 (8 + 1 + 0), Lck 40 (39 + 0 + 1, at the cap, drawn fine), HP 5/21 |
| Titania (Next Unit) Skill | Apply 20: the sheet reads Skill 24 (20 + class 4 + 0) for the other unit; Def 3 unchanged at 11 (3 + 8) |

Not verified: Lv, EXP, Def, Res, Spd, HP rows on the sheet (same mechanism, same offsets, not each applied and photographed); whether the person record (heap) is rebuilt from game data on a chapter load or saved with the card; Mv/Wt (class record: shipped as nothing because the only byte found is shared by the class); Hit/Avo/Atk etc. are derived.

## Research notes (not shipped: not verified)
- **Bonus EXP screen**: see the table above (the funds screen was reached with the maintainer's first save, copy of the card, `FEPOR_MEMCARD_A`).
- **Item uses**: item slot is 8 bytes (id word, uses byte at `+4`, flags `+5`; from the copy routine at `0x8001C604`, the public "infinite usage" code patches `lbz r0,4(r4)` there, which a recompiled game cannot take). The address of Ike's slot in the unit object was not found.
- **Unlimited movement / 100% hit**: Mov and Hit are derived. The info-window copy at `0x8033B144` (u16: Atk, Def, Res, Hit, Avo, ..., Dodge) shows Ike's Hit 173 but is a computed copy; the stored stat bytes were not identified (writing into the `0x802BBB50` record at guessed offsets crashed or blanked the game). Prologue enemies cannot be fought within the scripted runs (Boyd waits until Ike walks to him).
- **Unlock everything** (public decrypted code): `0x8033BFC9` = `0x3F` (trial maps), 15 bytes `1..15` from `0x8033BFCA` (bonus characters). Not shipped: no screen shows them without a save/title menu check.
- Debug display: byte `0x803759C8` = 1 draws the engine's process list (seen working through this cheat path).

## Limits
Heap addresses (Ike's HP) are valid for the prologue map layout seen; the condition keeps a wrong layout from being written but the cheat then does nothing. One comparison per write, no pointer chains.
