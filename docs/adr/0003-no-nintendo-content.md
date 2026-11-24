# 0003 No Nintendo-derived content in the repo

Status: accepted

The user provides the disc image. The tool extracts into a git-ignored output directory and generates code locally on every build. The repo holds only tooling and runtime. Disc readers are abstract: raw ISO/GCM and extracted folder first, CISO/RVZ later. A hash check of GFEP01 guards the version; other regions come later via version tables.

## Update: the Japanese disc (ticket 26)

`GFEJ01` is a second row of the version table, with `crates/core/data/GFEJ01.toml`. The Japanese main.dol has the same `.init` layout, so the two excludes are at the same addresses; the two HLE functions were found by matching the EU code in it (OSSaveContext: identical bytes, now at `0x80200350`; OSLoadContext: the same code with other loaded addresses, at `0x802003d0`). Nothing else in the runtime depended on the region (the TV format is chosen from the game ID's fourth letter).

Checked with the one disc available, the fan-translated one (Souen no Kiseki, T-En v0.1 by Shadowofchaos): `prepare` and `run` reach the Japanese warning, the title, the memory-card questions, the difficulty menu, the prologue map and its dialogues (headless, no audio, no save). The hash in the table is that disc's main.dol: an original Japanese disc may have another hash and is refused until someone adds it. Not checked: saves, chapter 1 onwards, audio, battles, and the American disc (GFEE01) which has not been seen.

Update: with a save made on the Japanese build (prologue played and saved in a real session, the second slot being the start of chapter 1), a scripted headless run from that card loads chapter 1, plays the introduction flyover and its dialogues, reaches the player phase and the map and victory-terms menu, with the tactical map displayed. An EU save does not load on the Japanese game (the file is `FIREEMBLEM8J`, and the contents are rejected even when relabelled). Still unchecked on GFEJ01: battles, audio, saving from chapter 1, later chapters. The same five out-of-step GX commands per run appear as on the EU disc (the line-strip draws of ADR 0013's follow-up), handled the same way.
