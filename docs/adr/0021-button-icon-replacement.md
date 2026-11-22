# 0021 Button icon replacement

Status: accepted (implements ticket #22)

**What**
- The map screen (bottom right, in the player phase) draws hints for the GameCube buttons as textures: A (a dot on a grey halo), B, X, Y and Z. When the player picks Xbox, Nintendo or PlayStation in the overlay's Controls page, these are replaced by an icon of the gamepad button the GameCube button is bound to (default Z on RB shows `RB` / `R` / `R1`). The GameCube family keeps the game's art. Rebinding or changing the family redraws the icons at once.
- Verified on the real game with frame dumps: Xbox (green A, red B, blue X, yellow Y, `RB` pill) and PlayStation (cross, circle, square, triangle on dark discs, `R1`).

**How**
- `crates/video/src/icons.rs` lists the textures by content fingerprint (size plus sampled bytes, the same value `FEPOR_DUMP_TEXTURES` puts in the file name, so new ones are found by dumping textures in the scene), with the GameCube button each stands for and how the replacement sits in the texture (`Shape`: the A button is drawn at 0.48 of the texture on a translucent halo). `Renderer::texture` swaps the decoded image for the generated one; the dump still writes the game's art.
- `crates/overlay/src/glyphs.rs` draws the icons at run time: anti-aliased discs and pills, symbols from distance functions, letters from the font bundled with egui (Hack, MIT). No console maker's artwork is shipped. `Shared::button_icon` picks the bound source (`Controls::config`) and the family; `Controls::generation` changes with the bindings or the family and the renderer drops its texture cache when it does. `cargo run -p fepor-overlay --example glyph_sheet -- out.png` writes a sheet of every generated icon.
- Nintendo names the buttons by position (SDL's `a`, south, is its B).

**Not done**
- The grey dot ("Cond") of the map hint, probably the C-stick: unknown which button it stands for.
- Icons inside text (dialogue, tutorials) are font glyphs of the game's atlases, drawn in grey levels only; not touched.
- Other scenes' icons (battle forecast, menus) when they use other textures: add their fingerprints to `icons::KNOWN`.
