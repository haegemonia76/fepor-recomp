# 0040 Text extraction and reinjection

Status: accepted

Translations and text fixes ship as a mod (ADR for the mod manager, ticket 34), never as extracted text in the repo (ADR 0003). `fepor-recomp text-extract` turns the player's message files into editable text; `fepor-recomp text-build` turns the edited text into the mod's replacement files.

## The disc's message files

`Mess/*.m`, 180 files: `c01`..`c31` (chapter dialogue), `common`, `ending`, `tut`, `videomode`, `yell`; English has no prefix, German `d_`, French `f_`, Italian `i_`, Spanish `s_`. Big-endian layout, identical in all of them:

| Offset | Content |
|---|---|
| 0x00 | u32 file size |
| 0x04 | u32 size `T` of the text area |
| 0x08 | u32 0 |
| 0x0c | u32 entry count `N` |
| 0x10 | 16 zero bytes |
| 0x20 | text area (`T` bytes): NUL-terminated strings, each padded with NULs to a multiple of 4 |
| 0x20+T | `N` pairs of u32: offset of the text in the text area, offset of the name in the names area |
| after | names area: NUL-terminated names in table order, no padding, up to the end of the file |

The table order is the message index the game uses; the text area is stored in another order (kept in the editable file). Texts are raw bytes: ASCII with `$` control codes (`$K` wait, `$w3`, `$c0NAME|`, ...), LF line breaks, single-byte accented letters and Shift-JIS runs; some names (`MG_...` in `common.m`) are Shift-JIS too. Treat every byte as opaque; the font's mapping of bytes >= 0x80 was not investigated.

The parser accepts a file only if rebuilding it gives the same bytes (canonical layout, no shared strings, no gaps). The other tables on the disc (`tut_data_*.bin`, `MovieCaptionData.bin`) hold identifiers and offsets, not message text, and are not covered.

## Editable form

`<dir>/Mess/<file>.m.txt`, UTF-8, LF or CRLF:

```
# fepor-recomp text v1: Mess/c01.m      <- comment lines before the first header are ignored
## 24 MS_01_OP_01                        <- table index, name
$R\x8F\xE3...$K                          <- text; a line break is a LF byte of the text
second line

## 25 ...
```

Entries are in text-area order. The body is the text's lines; a blank line ends every entry (so text ending in LF has an empty last line before it). Printable ASCII is literal; `\\` is a backslash, every other byte (including controls and >= 0x80) is `\xHH`; a body line starting with `#` is written `\#`. Names use the same escapes, plus `\x20` for a space. Non-ASCII characters are rejected: write the game's bytes as `\xHH`. Text may be longer or shorter than the original (the file is rebuilt with new offsets); the set of (index, name) pairs must equal the disc's, which `text-build` checks.

## Mod layout

`text-build <dir> --mod <mod>` writes `<mod>/files/Mess/<file>.m`: the path inside `files/` is the disc path, which the mod manager (ADR 0034) overlays on the disc's files. If `<mod>/mod.toml` does not exist, `text-build` writes one (`name` = the folder name, `game_id` = the disc's, a description saying it is generated); an existing one is never touched, so authors can add `author`/`version`. Put the mod folder in `<data>/mods/` and enable it in the launcher or overlay. Files that rebuild identical to the disc are skipped (`--all` writes them anyway), so a mod holds only what changed.

## Verified, not verified

Round trip on the EU disc: extract all, build all, 180 of 180 files byte-identical. Not verified: that the game accepts a replacement file of another size (a replaced file is served by the mod manager; see the PR for the headless check), and rendering of translated text.
