# 0016 Input layer and memory card

Status: accepted (amends ADR 0007)

**Memory card (CARD)**
- No CARD HLE: the SDK's CARD library is lifted guest code and talks to a memory card device on EXI channel 0/1 (device 0), implemented in `crates/runtime/src/card.rs` after Dolphin's EXI card device (ID, read array, page program, sector/chip erase, status, interrupt switch; DMA for bulk data). The game therefore formats the card, builds directory/BAT and writes saves itself.
- Image format: Dolphin's raw `.raw` (card contents byte for byte, `megabits * 131072` bytes; 4 Mbit = 59 blocks, 16 Mbit = 251). New cards are 16 Mbit and blank (0xFF); the game offers to format them. Programming only clears bits, erase sets a block to 0xFF, every write goes through to the file immediately.
- The OS keeps a flash ID derived from the card header in SRAM; it is recomputed from the image when the card is inserted (Dolphin's algorithm).
- EXI interrupts (device, transfer complete) now reach the PI (cause bit 4).
- Files: `<data>/memcards/MemoryCardA.<USA|EUR|JAP>.raw` (data = `--data`, default `$XDG_DATA_HOME/fepor-recomp`); `FEPOR_MEMCARD_A/B=<file|none>` override. `fepor-recomp card-info <image>` checks header/directory/BAT checksums and lists the saves, to verify images are structurally valid.
- Verified on the real disc: unformatted card detected ("needs to be formatted"), formatted by the game (16 Mbit, 251 free blocks, all checksums valid), `FIREEMBLEM8` save created (19 blocks).
- Saves are shared with Dolphin as `.gci` files (Dolphin's default GCI-folder cards): `fepor-recomp card-import` / `card-export` move them in and out of a raw image. Verified with `tools/dolphin-run.sh` and the owner's real save: Dolphin's `01-GFEP-FIREEMBLEM8.gci` imported into a raw card shows up in the recomp's file menu ("Ch. 1: The Battle Begins", 00:06:52), and exporting it again gives a byte-identical GCI. A save the recomp wrote itself (Copy File of that slot, through the emulated card) exported as GCI and loaded in Dolphin shows both slots.
- Not working / unexplained: handing Dolphin a raw `.raw` image (`SlotA = MemoryCard` + `MemcardAPath`) made its game report "no Fire Emblem file", even for an unmodified Dolphin save imported into the image, and Dolphin never wrote the path it was given; the GCI route is the supported one. A save file that was only created (never played into) is empty and is not listed by either emulator.

**Input**
- `crates/input`: a virtual GameCube pad built from named sources (keyboard `KeyZ`, `ArrowUp`, ...; SDL gamepad `a`, `dpup`, `leftx+`, `lefttrigger`, ...). Bindings and the glyph family live in `<data>/input.toml` (written with defaults on first run, loaded on start). Analog sources have a configurable dead zone; shoulder buttons click only at the end of their travel; the stick spans 128 +/- 100.
- Gamepads are read by SDL3 on a thread of their own (background events allowed). The state is published to the SI through an atomic (`si::set_pad`), as before.
- Remapping: first through the F2/F3/F4 hotkeys, now (ADR 0020) in the settings overlay (F1 / Guide): per-action rebinding of the keyboard and the gamepad (Esc keeps the current keyboard binding, or abandons a gamepad capture), button-name family and dead zone. Everything is saved to `input.toml`.
- Glyph families name the buttons in the overlay; the icons the game draws on the map are replaced at run time (ADR 0021).
- Gamepad hot-plug, one profile per controller model in `input.toml` and four players: see ADR 0038.
