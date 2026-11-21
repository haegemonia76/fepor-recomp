# 0016 Input layer and memory card

Status: accepted (amends ADR 0007)

**Memory card (CARD)**
- No CARD HLE: the SDK's CARD library is lifted guest code and talks to a memory card device on EXI channel 0/1 (device 0), implemented in `crates/runtime/src/card.rs` after Dolphin's EXI card device (ID, read array, page program, sector/chip erase, status, interrupt switch; DMA for bulk data). The game therefore formats the card, builds directory/BAT and writes saves itself.
- Image format: Dolphin's raw `.raw` (card contents byte for byte, `megabits * 131072` bytes; 4 Mbit = 59 blocks, 16 Mbit = 251). New cards are 16 Mbit and blank (0xFF); the game offers to format them. Programming only clears bits, erase sets a block to 0xFF, every write goes through to the file immediately.
- The OS keeps a flash ID derived from the card header in SRAM; it is recomputed from the image when the card is inserted (Dolphin's algorithm).
- EXI interrupts (device, transfer complete) now reach the PI (cause bit 4).
- Files: `<data>/memcards/MemoryCardA.<USA|EUR|JAP>.raw` (data = `--data`, default `$XDG_DATA_HOME/fepor-recomp`); `FEPOR_MEMCARD_A/B=<file|none>` override. `fepor-recomp card-info <image>` checks header/directory/BAT checksums and lists the saves, to verify images are structurally valid.
- Verified on the real disc: unformatted card detected ("needs to be formatted"), formatted by the game (16 Mbit, 251 free blocks, all checksums valid), `FIREEMBLEM8` save created (19 blocks). Not verified: loading these images in Dolphin / a console, and loading Dolphin-made images here.

**Input**
- `crates/input`: a virtual GameCube pad built from named sources (keyboard `KeyZ`, `ArrowUp`, ...; SDL gamepad `a`, `dpup`, `leftx+`, `lefttrigger`, ...). Bindings and the glyph family live in `<data>/input.toml` (written with defaults on first run, loaded on start). Analog sources have a configurable dead zone; shoulder buttons click only at the end of their travel; the stick spans 128 +/- 100.
- Gamepads are read by SDL3 on a thread of their own (background events allowed). The state is published to the SI through an atomic (`si::set_pad`), as before.
- Remapping in the window: F2 remaps the keyboard, F3 the gamepad (each GameCube control in turn, Esc keeps the current binding; progress is shown in the window title), F4 cycles the glyph family (GameCube, Xbox, Nintendo, PlayStation). Everything is saved.
- Glyph families are names/labels for now (`GlyphSet::source_label`); replacing the button icons the game draws needs replacement textures keyed by content hash (`FEPOR_DUMP_TEXTURES` finds them) and art, and is not done. The egui front end (M5.3) is the place for a graphical remapping page.
