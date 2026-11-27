//! Native cheats (ADR 0033): named, readable cheats of one game written as TOML, not tied to the Gecko format.
//!
//! ```toml
//! [[cheat]]
//! name = "Max gold"
//! description = "Gold stays at the cap."   # optional, shown under the name
//! game = "GFEP01"                          # optional: skipped for another game
//! continuous = true                        # held every frame; absent/false: written once when turned on
//!
//! [[cheat.write]]
//! address = 0x80123456                     # cached, uncached or physical form of a main RAM address
//! size = 4                                 # 1, 2 or 4 bytes, big-endian (default 4)
//! value = 99999
//!
//! [cheat.write.when]                       # optional: write only while this comparison holds
//! address = 0x80123450
//! size = 1                                 # default 4
//! op = "ne"                                # eq (default), ne, lt, le, gt, ge
//! value = 0
//! ```
//!
//! The cheats of the repository are in `crates/core/cheats/<game id>.toml` (addresses, sizes and values only, no
//! game data, ADR 0003) and compiled in; the player can add or replace cheats (by name) in
//! `<data>/cheats/<game id>.toml`. One cheat that cannot be read (a typo, an address outside RAM) is listed as
//! unusable with the reason; the others still load. The writes go through the same engine as the Gecko codes
//! (`fepor_core::cheats::CheatStore`, `fepor_runtime::cheats::CheatRunner`).

use std::path::Path;

use serde::Deserialize;

use crate::cheats::{Cheat, Cmp, Condition, Kind, Write};

/// Main RAM size of the GameCube.
const RAM_SIZE: u32 = 0x0180_0000;

/// The cheats shipped for each game ID.
const SHIPPED: &[(&str, &str)] = &[("GFEP01", include_str!("../cheats/GFEP01.toml"))];

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    cheat: Vec<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    name: String,
    game: Option<String>,
    description: Option<String>,
    #[serde(default)]
    continuous: bool,
    #[serde(default)]
    write: Vec<RawWrite>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWrite {
    address: i64,
    #[serde(default = "four")]
    size: u8,
    value: i64,
    when: Option<RawWhen>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWhen {
    address: i64,
    #[serde(default = "four")]
    size: u8,
    #[serde(default = "eq")]
    op: String,
    value: i64,
}

fn four() -> u8 {
    4
}

fn eq() -> String {
    "eq".into()
}

/// A main RAM address in cached, uncached or physical form, as the cached `0x80…` address of `size` bytes.
fn ram_address(a: i64, size: u8) -> Result<u32, String> {
    let a = u32::try_from(a).map_err(|_| format!("address {a:#x} is not a 32-bit value"))?;
    // physical (below 32 MiB), cached 0x80.. / 0x81.. or uncached 0xC0.. / 0xC1..
    if !matches!(a >> 25, 0 | 0x40 | 0x60) {
        return Err(format!("address {a:#010x} is not in main RAM"));
    }
    let p = a & 0x01FF_FFFF;
    if p as u64 + size as u64 > RAM_SIZE as u64 {
        return Err(format!("address {a:#010x} is not in main RAM"));
    }
    Ok(0x8000_0000 | p)
}

fn width(size: u8) -> Result<(), String> {
    match size {
        1 | 2 | 4 => Ok(()),
        n => Err(format!("size {n} is not 1, 2 or 4")),
    }
}

fn value(v: i64, size: u8) -> Result<u32, String> {
    let max = if size == 4 { u32::MAX as i64 } else { (1i64 << (8 * size)) - 1 };
    if (0..=max).contains(&v) {
        Ok(v as u32)
    } else {
        Err(format!("value {v} does not fit {size} byte(s)"))
    }
}

fn compile(w: RawWrite) -> Result<Write, String> {
    width(w.size)?;
    let v = value(w.value, w.size)?;
    let when = w
        .when
        .map(|c| {
            width(c.size)?;
            let cmp = match c.op.as_str() {
                "eq" => Cmp::Eq,
                "ne" => Cmp::Ne,
                "lt" => Cmp::Lt,
                "le" => Cmp::Le,
                "gt" => Cmp::Gt,
                "ge" => Cmp::Ge,
                other => return Err(format!("condition op {other:?} is not eq, ne, lt, le, gt or ge")),
            };
            Ok(Condition {
                addr: ram_address(c.address, c.size)?,
                size: c.size,
                cmp,
                value: value(c.value, c.size)?,
            })
        })
        .transpose()?;
    Ok(Write {
        addr: ram_address(w.address, w.size)?,
        bytes: v.to_be_bytes()[4 - w.size as usize..].to_vec(),
        when,
    })
}

fn compile_cheat(raw: Raw) -> Result<Vec<Write>, String> {
    if raw.write.is_empty() {
        return Err("the cheat has no [[cheat.write]]".into());
    }
    raw.write.into_iter().map(compile).collect()
}

/// The cheats of a TOML text for `game`. `Err` only when the text is not TOML at all; a cheat that cannot be
/// used is returned with the reason (`writes: Err`). Cheats naming another game are left out.
pub fn parse(text: &str, game: &str) -> Result<Vec<Cheat>, String> {
    let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (i, v) in file.cheat.into_iter().enumerate() {
        let name = v.get("name").and_then(|n| n.as_str()).map(str::to_owned);
        let raw: Result<Raw, _> = v.try_into();
        let cheat = match raw {
            Ok(raw) => {
                if raw.game.as_deref().is_some_and(|g| g != game) {
                    continue;
                }
                Cheat {
                    kind: Kind::Native,
                    name: raw.name.clone(),
                    author: String::new(),
                    notes: raw.description.iter().cloned().collect(),
                    lines: Vec::new(),
                    continuous: raw.continuous,
                    writes: compile_cheat(raw),
                }
            }
            Err(e) => Cheat {
                kind: Kind::Native,
                name: name.unwrap_or_else(|| format!("(cheat #{})", i + 1)),
                author: String::new(),
                notes: Vec::new(),
                lines: Vec::new(),
                continuous: false,
                writes: Err(e.message().to_string()),
            },
        };
        out.push(cheat);
    }
    Ok(out)
}

/// Put `more` after `base`, a cheat of the same name replacing the one it names.
fn merge(base: &mut Vec<Cheat>, more: Vec<Cheat>) {
    for c in more {
        match base.iter_mut().find(|b| b.name == c.name) {
            Some(b) => *b = c,
            None => base.push(c),
        }
    }
}

/// The native cheats of `game`: the shipped ones, then those of the player's file `user` (replacing by name). The
/// second value says why the player's file was not read, when it was not.
pub fn load(game: &str, user: &Path) -> (Vec<Cheat>, Option<String>) {
    let mut out = Vec::new();
    for (id, text) in SHIPPED {
        if *id == game {
            match parse(text, game) {
                Ok(c) => out = c,
                // a bug of the repository, not of the player: shown like the player's errors
                Err(e) => return (out, Some(format!("the shipped cheats are broken: {e}"))),
            }
        }
    }
    let error = match std::fs::read_to_string(user) {
        Ok(text) => match parse(&text, game) {
            Ok(c) => {
                merge(&mut out, c);
                None
            }
            Err(e) => Some(format!("{}: {e}", user.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(format!("cannot read {}: {e}", user.display())),
    };
    (out, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = r#"
[[cheat]]
name = "Money"
description = "gold at the cap"
continuous = true
[[cheat.write]]
address = 0x80123456
size = 4
value = 99999

[[cheat]]
name = "Conditional"
game = "GFEP01"
[[cheat.write]]
address = 0xC0001000
size = 2
value = 0x270F
[cheat.write.when]
address = 0x80001002
size = 1
op = "ne"
value = 0
[[cheat.write]]
address = 0x1010
value = 0

[[cheat]]
name = "Other game"
game = "GFEJ01"
[[cheat.write]]
address = 0x80001000
value = 1

[[cheat]]
name = "Too big"
[[cheat.write]]
address = 0x80001000
size = 1
value = 256

[[cheat]]
name = "Outside"
[[cheat.write]]
address = 0x81800000
value = 1

[[cheat]]
name = "Typo"
[[cheat.write]]
adress = 0x80001000
value = 1

[[cheat]]
name = "Empty"

[[cheat]]
name = "Bad op"
[[cheat.write]]
address = 0x80001000
value = 1
[cheat.write.when]
address = 0x80001004
op = "approx"
value = 1
"#;

    fn find<'a>(c: &'a [Cheat], n: &str) -> &'a Cheat {
        c.iter().find(|c| c.name == n).unwrap_or_else(|| panic!("{n}"))
    }

    #[test]
    fn writes_conditions_and_the_game_filter() {
        let c = parse(TEXT, "GFEP01").unwrap();
        assert!(c.iter().all(|c| c.kind == Kind::Native));
        assert!(c.iter().all(|c| c.name != "Other game"));
        let m = find(&c, "Money");
        assert_eq!(m.key(), "native:Money");
        assert_eq!(m.notes, ["gold at the cap"]);
        assert!(m.continuous);
        assert_eq!(m.writes, Ok(vec![Write::new(0x8012_3456, vec![0, 1, 0x86, 0x9F])]));
        let k = find(&c, "Conditional");
        assert!(!k.continuous);
        let w = k.writes.as_ref().unwrap();
        // the uncached and the physical forms become the cached address
        assert_eq!(w[0].addr, 0x8000_1000);
        assert_eq!(w[0].bytes, [0x27, 0x0F]);
        assert_eq!(
            w[0].when,
            Some(Condition { addr: 0x8000_1002, size: 1, cmp: Cmp::Ne, value: 0 })
        );
        assert_eq!(w[1], Write::new(0x8000_1010, vec![0; 4]));
        let other = parse(TEXT, "GFEJ01").unwrap();
        assert!(other.iter().any(|c| c.name == "Other game"));
        assert!(other.iter().all(|c| c.name != "Conditional"));
    }

    #[test]
    fn a_bad_cheat_is_listed_unusable_and_the_rest_loads() {
        let c = parse(TEXT, "GFEP01").unwrap();
        let why = |n: &str| find(&c, n).writes.clone().unwrap_err();
        assert!(why("Too big").contains("does not fit 1 byte"), "{}", why("Too big"));
        assert!(why("Outside").contains("not in main RAM"));
        assert!(why("Typo").contains("adress"), "{}", why("Typo"));
        assert!(why("Empty").contains("no [[cheat.write]]"));
        assert!(why("Bad op").contains("approx"));
        assert!(find(&c, "Money").writes.is_ok());
    }

    #[test]
    fn text_that_is_not_toml_is_an_error() {
        assert!(parse("[[cheat]\nname =", "GFEP01").is_err());
        assert_eq!(parse("", "GFEP01").unwrap(), []);
    }

    #[test]
    fn the_memory_watcher_snippet_is_a_native_cheat() {
        use crate::memwatch::{cheat_toml, Width};
        let t = cheat_toml("Found", Some("GFEP01"), 0x8012_3456, Width::U16, 99, true);
        let c = parse(&t, "GFEP01").unwrap();
        assert_eq!(c.len(), 1);
        assert!(c[0].continuous);
        assert_eq!(c[0].writes, Ok(vec![Write::new(0x8012_3456, vec![0, 99])]));
    }

    #[test]
    fn the_player_file_adds_and_replaces_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("GFEP01.toml");
        std::fs::write(
            &f,
            "[[cheat]]\nname = \"Mine\"\n[[cheat.write]]\naddress = 0x80001000\nvalue = 1\n",
        )
        .unwrap();
        let (c, err) = load("GFEP01", &f);
        assert!(err.is_none());
        assert!(c.iter().any(|c| c.name == "Mine"));
        assert!(!c.is_empty());
        std::fs::write(&f, "[[cheat]\n").unwrap();
        let (c2, err) = load("GFEP01", &f);
        assert!(err.unwrap().contains("GFEP01.toml"));
        assert!(!c2.iter().any(|c| c.name == "Mine"));
        let (c3, err) = load("GFEP01", &dir.path().join("missing.toml"));
        assert!(err.is_none());
        assert_eq!(c3.len(), c2.len());
        let mut base = vec![find(&parse(TEXT, "GFEP01").unwrap(), "Money").clone()];
        let other = parse("[[cheat]]\nname = \"Money\"\n[[cheat.write]]\naddress = 0x80001000\nvalue = 1\n", "GFEP01")
            .unwrap();
        merge(&mut base, other);
        assert_eq!(base.len(), 1);
        assert_eq!(base[0].writes.as_ref().unwrap()[0].addr, 0x8000_1000);
    }

    #[test]
    fn shipped_cheats_are_valid_and_unique() {
        for (game, text) in SHIPPED {
            let c = parse(text, game).unwrap();
            let mut names: Vec<_> = c.iter().map(|c| c.name.as_str()).collect();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), c.len(), "{game}: duplicate names");
            for c in &c {
                assert!(c.writes.is_ok(), "{game}: {}: {:?}", c.name, c.writes);
            }
        }
    }
}
