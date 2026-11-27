# 0048 Memory watcher (developer overlay page)

Status: accepted (ticket #48; builds on ADR 0025 patch engine, ADR 0020 overlay)

A developer page in the overlay reads and writes guest addresses, shows values live, searches RAM (equals X / changed / did not change / increased / decreased between steps) and exports an address as a native-cheat TOML entry or a Gecko code line. It exists to find the addresses for cheats (#32, #33).

## Access to guest RAM: the guest thread only

The overlay runs on the render thread; guest RAM belongs to the guest thread and is written without locks by recompiled code (ADR 0025). The page therefore never touches RAM. `fepor_core::memwatch::Link` (in `overlay::Shared::memory`, handed to the machine by `main.rs`) carries:

- **requests** UI → guest (`Request`: new scan, narrow, poke/hold/release, hex-view window, watch list), a mutex-guarded queue;
- **published copies** guest → UI (`Published`: scan count and first 200 hits with values now, the 128-byte view window, watched values, held values, the last message), replaced as a whole under a mutex.

The guest side is `fepor_runtime::watch::Watcher`, served from `PatchEngine::frame` (the per-frame hook, ADR 0025: between guest instructions, once per frame). It drains the requests, then publishes. A request thus takes effect at the next frame boundary, and every value shown is a consistent copy from one instant. A scan step compares the RAM at that frame with the RAM at the step before, which is what "between frames" means.

Cost when unused: with no request and the page not shown (`Link::is_active`, set by the overlay only while the Memory page is drawn) the hook does one mutex lock on an empty queue per frame. A scan keeps a 24 MiB copy of RAM and, once narrowed, a list of candidate offsets; a step is one linear pass done inside one hook run (a few ms for the first full pass).

## Writes go through the patch engine when they must last

- **Write**: one store into RAM in the hook; the game may overwrite it.
- **Hold**: a one-patch `continuous` set with source `"watch"`, registered and enabled in the engine, so it is rewritten every frame, conflicts with other sources' enabled sets are reported (`cannot hold … overlaps …`) and **Release** reverts it (the bytes of the moment of holding return, ADR 0025). Needs a selected game (`set_game`).
- Addresses must be in main RAM (cached, uncached or physical form); others are refused with a message. The same limit as ADR 0025 applies: data only, patching code changes nothing.

## Search

`memwatch::Scanner`: width 8/16/32 bit big-endian, addresses aligned to the width. A scan starts either with a value (`equals`) or with "unknown" (every aligned address is a candidate, stored implicitly); each step keeps the candidates for which `Equal(x) | Changed | Unchanged | Increased | Decreased` holds, comparing unsigned against the value at the previous step. Pure over a RAM slice, tested on synthetic RAM.

## Export formats

For an address, width and value:

- **Gecko** RAM write: `00AAAAAA 000000VV`, `02AAAAAA 0000VVVV`, `04AAAAAA VVVVVVVV`, with `AAAAAA` the low 25 bits of the address (bit 24 selects the `0x81…` half, giving `01/03/05`).
- **Native cheat TOML** (the format of #33 did not exist when this was written; this is the snippet the page produces, #33 may adopt it or adapt the exporter in `memwatch::cheat_toml`):

```toml
[[cheat]]
name = "Found value"
game = "GFEP01"          # when known
continuous = true        # only when "hold every frame" is ticked

[[cheat.write]]
address = 0x802b0e94     # cached address in the game's RAM
size = 4                 # 1, 2 or 4 bytes, big-endian
value = 0x00000063
```

## Developer option and scripting

`settings.toml` `developer = true` (checkbox on the Help page, or `FEPOR_DEVELOPER=1` for one run) adds the Memory tab; `FEPOR_OVERLAY=memory` opens it directly. For headless runs and the documentation, `FEPOR_MEMSCRIPT` runs steps in the words of the buttons, separated by `;`, one every 45 painted frames (a frame is painted per presented/dumped image): `new u32 unknown`, `new u16 equal 5`, `changed`, `unchanged`, `inc`, `dec`, `equal N`, `watch ADDR [width]`, `view ADDR`, `poke ADDR width VALUE [hold]`, `select ADDR width [value]`.

**Live steps.** `FEPOR_MEMSCRIPT_FILE=<path>` is polled every 10 painted images; each new whole line appended since the last read is one step (same words as above, plus `clear`, `release ADDR`, and `write`/`hold ADDR width VALUE`; blank lines and `#` comments ignored), run one per 10 images. Steps run whether or not the overlay or the page is shown (the guest hook serves requests regardless). The page's state is rewritten to `<path>.state` when it changes (`game`, `message`, `candidates N`, up to 8 `candidate ADDR before B now N`, `watch ADDR width value`, `held ADDR value`), and the guest keeps publishing while the variable is set, so a script can drive a game it has to play to a given point and read results without screenshots.

## Limits

No floats or strings, no breakpoints (a "light" debugger: it cannot stop the game), no pointer chains. The watch list and hex view are not persisted. The 200 published hits are the lowest addresses; the page lists the first few (narrow the search to see others). Candidates in the 32-bit search include timers and counters; finding the right one needs a few meaningful steps in the game.
