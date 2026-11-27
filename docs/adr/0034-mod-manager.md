# 0034 Mod manager: disc file replacement and memory patches

Status: accepted (ticket #34; builds on the patch engine, ADR 0025)

A **mod** is a folder `<data>/mods/<id>/` (the folder name is its identity). It changes the game in two ways, both applied when the game **starts**; changing the list while playing takes effect at the next start.

## Mod format

```
<data>/mods/<id>/mod.toml
<data>/mods/<id>/files/<path on the disc>     optional replacement files
<data>/mods/mods.toml                          priority order and enabled mods (written by the GUI/overlay)
```

`mod.toml`:

```toml
name = "Readable name"
author = "…"             # optional
version = "1.0"          # optional
game_id = "GFEP01"       # required: a mod does nothing for another game
description = "…"        # optional
continuous = false       # optional: write the patches every frame (a held value) instead of once at start

[[patch]]                # optional, any number
address = 0x80410000     # cached, uncached or physical address, in main RAM
bytes = "00 00 00 01"    # hex, spaces optional
```

`files/` mirrors the disc's file system: `files/dir/b.bin` replaces the disc file `dir/b.bin`. Only files the disc already has can be replaced (a name the disc lacks is reported and ignored): adding files would change the FST's size, which the guest reads from its header.

**A mod never ships Nintendo-derived content (ADR 0003).** It contains only what its author made: modified or new data for the files of the player's own disc, as a replacement they distribute on their own. Do not put the game's original files, textures or text into a mod or its archive. Gecko codes are not read here; they belong to the cheats (#32) and would use the same patch engine.

## File replacement: `ModdedDisc`

`fepor_core::disc::ModdedDisc` wraps any `Disc`. The guest gets its FST from `Disc::sys_files()` (`boot.rs`) and reads by raw disc offset (`di.rs` → `Disc::read_at`), so the wrapper:

- appends each replaced file **after the end of the image** (`Disc::image_len()`, 32-byte aligned, in priority order), so no original byte moves and no existing offset changes;
- rewrites the FST (`rewrite_fst`): only the offset and size words of the replaced entries change, names, directory structure and table length stay byte for byte;
- serves reads inside the appended range from the host files (a read that straddles the end of the image gets the image's bytes and then the replacement; the gap between files reads as zero) and passes every other read to the wrapped disc untouched;
- lists the new offsets in `files()` and serves `read_file` from the host for them.

The host file's size is read when the game starts; a file shorter than that when read gives zeros for the rest. The 4 GiB limit of the FST's 32-bit offsets is checked.

## Priority and conflicts

The order in `mods.toml` is the priority: the first mod wins. Only enabled mods (for the running game) count.

- **Same disc file**: the higher priority mod's file is used; the conflict is listed.
- **Same bytes** (patches compared as physical addresses, so the cached and uncached forms of an address overlap): the engine refuses the lower priority mod's whole set (`PatchError::Conflict`), as ADR 0025 defines; it stays off and the game prints why. The list shows the same overlap beforehand (`ModList::conflicts`).

Conflicts are shown in the launcher GUI and in the overlay's Mods page, and printed at start (`mods: …`).

## Where it lives

- `crates/core/src/mods.rs`: the format, `ModList` (load, order, enabled, save, conflicts, replacement list, patch mods);
- `crates/core/src/disc.rs`: `ModdedDisc`, `rewrite_fst`, `Disc::image_len`;
- `crates/game/src/main.rs`: wraps the disc and registers the patch sets (`mod:<id>`) after boot, so the DOL's data is already in RAM;
- the overlay's Mods page (`crates/overlay/src/mods.rs`) and the launcher GUI's Mods section edit `mods.toml`.

## Limits

- Everything applies at the next start; a mod cannot be switched live. (File replacement cannot: the FST is read at boot. Patches could, through the engine, but one rule is easier to explain.)
- Patches are data only (ADR 0025): a patch over the DOL's code is reported and changes nothing. Code the game loads from the disc is not recompiled (ADR 0001), so file mods change data (models, text, tables, sound), not behaviour in code.
- Texture replacement (#27) is separate: not handled here.
