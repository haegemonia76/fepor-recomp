# 0018 The output folder is the game

Status: accepted (amends ADR 0011)

- Once `fepor-recomp prepare` has finished, the game must run without the disc image. The recompiled code is only half of it: textures, models, text, sounds and movies are streamed from the disc while playing, through the emulated DVD drive.
- `prepare` therefore also copies the disc's `sys/` and `files/` (Dolphin folder layout) to `<data>/disc` on the player's machine (about 1.3 GB; progress 0.65-0.99; existing files of the right size are kept, so an interrupted copy resumes). `prepared.json` is written last and names the folder (`"disc": "disc"`). Nothing Nintendo is shipped: the copy is made from the player's own disc (ADR 0003).
- `fepor-game` takes `--data <dir>` and, without `--game`, reads the game from `<data>/disc` (the folder disc reader maps the DVD's byte offsets onto the extracted files). `--game <iso|folder>` still overrides, for development.
- Without `--data` the data folder is `RECOMPLAUNCHER_DATA`, else the output folder the launcher GUI last used (when a finished build is in it), else `$XDG_DATA_HOME/fepor-recomp`. Memory cards (`memcards/`) and `input.toml` live in the same folder.
- RecompLauncher: `start` runs `fepor-game --data {data}`; the game path is only needed for `prepare`.
- Verified: a fresh `prepare` (38 s on this machine, the build being cached) then `fepor-game --data <dir>` with no disc argument played the Nintendo logo and the opening movie from the extracted files.
- Still open (ticket #16): the compiled game code is linked into `fepor-game` at build time by the development backend; the production single executable generates it in process.
