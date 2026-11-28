//! The character a game's status screen shows, and the stats of that character as the Cheats page edits them
//! (ADR 0033, ticket #66).
//!
//! The game keeps one global word that points at the unit object it last displayed (status sheet, map hover
//! window). A unit object points at its *person* record (the character's own stat bonuses) and its *job* record
//! (class base stats and caps); the sheet shows `min(person + job + unit gains, cap)`. A [`CharDef`] (TOML,
//! `[character]` in the cheat files) says where each of these lives; this module resolves the selected unit on a
//! RAM image, breaks each stat into its parts and writes the one part the cheat owns (the *base*, a field of the
//! character's own record), clamped to the range the row allows. Nothing is held and nothing is reverted.

use serde::Deserialize;

/// Where a byte lives: at `off` in the unit object, its person record or its job record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Loc {
    pub rec: Rec,
    pub off: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rec {
    Unit,
    Person,
    Job,
}

/// One row of the character table. `base` is the only thing a cheat writes (one signed byte); `class` and
/// `other` are read to show the breakdown and to compute the total the sheet shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub base: Loc,
    pub class: Option<Loc>,
    pub other: Option<Loc>,
    /// The class cap of this stat; a byte of 0 means no cap is stored.
    pub cap: Option<Loc>,
    /// Lowest base that can be written.
    pub min: i32,
    /// Highest total the row accepts when the game stores no cap (and an upper limit besides it).
    pub max_total: i32,
    /// The highest total is that of another row (HP now never exceeds max HP).
    pub max_total_of: Option<String>,
    /// Shown as a tag: what the row does not cover.
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharDef {
    /// The global word holding the address of the displayed unit object.
    pub unit_ptr: u32,
    /// Offsets in the unit object of the person and job record pointers.
    pub person_at: u32,
    pub job_at: u32,
    /// Globals that hold the same two record pointers for the displayed unit: they must agree with the unit's own.
    pub person_check: Option<u32>,
    pub job_check: Option<u32>,
    pub rows: Vec<Row>,
}

/// A selected unit on a RAM image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sel {
    pub unit: u32,
    pub person: u32,
    pub job: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowView {
    pub label: String,
    pub base: i32,
    pub class: Option<i32>,
    pub other: Option<i32>,
    /// What the sheet shows.
    pub total: i32,
    /// The cap in force (stored class cap or the row's own limit).
    pub cap: i32,
    /// Allowed range of the base.
    pub min: i32,
    pub max: i32,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharView {
    pub name: String,
    pub class: String,
    pub rows: Vec<RowView>,
}

const RAM: u32 = 0x0180_0000;

fn phys(a: u32) -> Option<usize> {
    (a >> 24 == 0x80 || a >> 24 == 0x81).then_some((a & 0x01FF_FFFF) as usize).filter(|p| *p < RAM as usize)
}

fn byte(ram: &[u8], a: u32) -> Option<u8> {
    ram.get(phys(a)?).copied()
}

fn word(ram: &[u8], a: u32) -> Option<u32> {
    let p = phys(a)?;
    ram.get(p..p + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// The text of a game id string such as `PID_IKE` / `JID_RANGER` / `JID_TIAMAT/F` as a name: prefix and suffix
/// dropped, `_` to spaces, first letters in capitals.
pub fn id_name(raw: &[u8], prefix: &str) -> String {
    let s: String = raw.iter().take_while(|b| **b != 0 && **b != b'/').map(|b| *b as char).collect();
    let s = s.strip_prefix(prefix).unwrap_or(&s);
    s.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn id_at(ram: &[u8], record: u32, prefix: &str) -> Option<String> {
    let s = word(ram, record)?;
    let p = phys(s)?;
    let raw = ram.get(p..(p + 32).min(ram.len()))?;
    raw.starts_with(prefix.as_bytes()).then(|| id_name(raw, prefix))
}

impl CharDef {
    /// The displayed unit, or why there is none (no sheet shown yet, pointers that do not agree, stale object).
    pub fn resolve(&self, ram: &[u8]) -> Result<Sel, String> {
        let none = || "Open a character sheet (Y on the map)".to_string();
        let unit = word(ram, self.unit_ptr).filter(|u| phys(*u).is_some()).ok_or_else(none)?;
        let person = word(ram, unit.wrapping_add(self.person_at)).ok_or_else(none)?;
        let job = word(ram, unit.wrapping_add(self.job_at)).ok_or_else(none)?;
        if self.person_check.is_some_and(|a| word(ram, a) != Some(person))
            || self.job_check.is_some_and(|a| word(ram, a) != Some(job))
        {
            return Err(none());
        }
        if id_at(ram, person, "PID_").is_none() || id_at(ram, job, "JID_").is_none() {
            return Err(none());
        }
        Ok(Sel { unit, person, job })
    }

    fn at(&self, sel: Sel, l: Loc) -> u32 {
        let base = match l.rec {
            Rec::Unit => sel.unit,
            Rec::Person => sel.person,
            Rec::Job => sel.job,
        };
        base.wrapping_add(l.off)
    }

    fn signed(&self, ram: &[u8], sel: Sel, l: Loc) -> Option<i32> {
        byte(ram, self.at(sel, l)).map(|b| b as i8 as i32)
    }

    fn view_row(&self, ram: &[u8], sel: Sel, r: &Row, totals: &[RowView]) -> Option<RowView> {
        let base = self.signed(ram, sel, r.base)?;
        let class = r.class.and_then(|l| self.signed(ram, sel, l));
        let other = r.other.and_then(|l| self.signed(ram, sel, l));
        let stored = r.cap.and_then(|l| byte(ram, self.at(sel, l))).filter(|c| *c > 0).map(i32::from);
        let mut cap = stored.map_or(r.max_total, |c| c.min(r.max_total));
        if let Some(of) = &r.max_total_of {
            cap = cap.min(totals.iter().find(|t| &t.label == of)?.total);
        }
        let rest = class.unwrap_or(0) + other.unwrap_or(0);
        let total = (base + rest).min(cap);
        let max = (cap - rest).max(r.min);
        Some(RowView {
            label: r.label.clone(),
            base,
            class,
            other,
            total,
            cap,
            min: r.min,
            max,
            note: r.note.clone(),
        })
    }

    /// The name, class and every row of the displayed unit.
    pub fn view(&self, ram: &[u8]) -> Result<CharView, String> {
        let sel = self.resolve(ram)?;
        let mut rows: Vec<RowView> = Vec::new();
        for r in &self.rows {
            rows.push(self.view_row(ram, sel, r, &rows).ok_or("the unit's record is outside main RAM")?);
        }
        Ok(CharView {
            name: id_at(ram, sel.person, "PID_").unwrap_or_default(),
            class: id_at(ram, sel.job, "JID_").unwrap_or_default(),
            rows,
        })
    }

    /// Write `typed` as the base of the row `label` of the displayed unit, clamped to the row's range.
    /// Returns the row as it reads afterwards.
    pub fn apply(&self, ram: &mut [u8], label: &str, typed: i64) -> Result<RowView, String> {
        let sel = self.resolve(ram)?;
        let i = self.rows.iter().position(|r| r.label == label).ok_or_else(|| format!("no row {label:?}"))?;
        let done: Vec<RowView> = {
            let mut v = Vec::new();
            for r in &self.rows[..i] {
                v.push(self.view_row(ram, sel, r, &v).ok_or("outside main RAM")?);
            }
            v
        };
        let now = self.view_row(ram, sel, &self.rows[i], &done).ok_or("the unit's record is outside main RAM")?;
        let value = typed.clamp(now.min as i64, now.max as i64) as i8;
        let a = phys(self.at(sel, self.rows[i].base)).ok_or("outside main RAM")?;
        *ram.get_mut(a).ok_or("outside main RAM")? = value as u8;
        let mut v = done;
        v.push(now);
        self.view_row(ram, sel, &self.rows[i], &v[..i]).ok_or_else(|| "outside main RAM".into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawChar {
    pub game: Option<String>,
    pub unit: i64,
    pub person_at: u32,
    pub job_at: u32,
    pub person_check: Option<i64>,
    pub job_check: Option<i64>,
    #[serde(default)]
    pub row: Vec<RawRow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRow {
    pub label: String,
    pub base: Loc,
    pub class: Option<Loc>,
    pub other: Option<Loc>,
    pub cap: Option<Loc>,
    #[serde(default)]
    pub min: i32,
    pub max_total: i32,
    pub max_total_of: Option<String>,
    #[serde(default)]
    pub note: String,
}

fn ram_word(a: i64) -> Result<u32, String> {
    let a = u32::try_from(a).map_err(|_| format!("{a:#x} is not a 32-bit address"))?;
    phys(a).map(|_| a).ok_or_else(|| format!("address {a:#010x} is not in main RAM"))
}

/// Compile a `[character]` table; refuses a row that limits itself by a row that comes after it.
pub fn compile(raw: RawChar) -> Result<CharDef, String> {
    let mut rows: Vec<Row> = Vec::new();
    for r in raw.row {
        if r.min < -128 || r.max_total > 127 || r.min > r.max_total {
            return Err(format!("row {}: min {} and max_total {} must fit a signed byte", r.label, r.min, r.max_total));
        }
        if rows.iter().any(|x| x.label == r.label) {
            return Err(format!("row {} is listed twice", r.label));
        }
        if let Some(of) = &r.max_total_of {
            if !rows.iter().any(|x| &x.label == of) {
                return Err(format!("row {}: max_total_of {of:?} must name an earlier row", r.label));
            }
        }
        rows.push(Row {
            label: r.label,
            base: r.base,
            class: r.class,
            other: r.other,
            cap: r.cap,
            min: r.min,
            max_total: r.max_total,
            max_total_of: r.max_total_of,
            note: r.note,
        });
    }
    if rows.is_empty() {
        return Err("the [character] table has no rows".into());
    }
    Ok(CharDef {
        unit_ptr: ram_word(raw.unit)?,
        person_at: raw.person_at,
        job_at: raw.job_at,
        person_check: raw.person_check.map(ram_word).transpose()?,
        job_check: raw.job_check.map(ram_word).transpose()?,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = r#"
unit = 0x80001000
person_at = 0x10
job_at = 0x14
person_check = 0x80001004
job_check = 0x80001008
[[row]]
label = "HP"
base = { rec = "person", off = 4 }
class = { rec = "job", off = 4 }
other = { rec = "unit", off = 0x20 }
cap = { rec = "job", off = 12 }
max_total = 99
[[row]]
label = "Str"
base = { rec = "person", off = 5 }
class = { rec = "job", off = 5 }
other = { rec = "unit", off = 0x21 }
cap = { rec = "job", off = 13 }
max_total = 40
[[row]]
label = "HP now"
base = { rec = "unit", off = 0x30 }
min = 1
max_total = 99
max_total_of = "HP"
"#;

    fn def4() -> CharDef {
        compile(toml::from_str(TOML).unwrap()).unwrap()
    }

    /// Unit at 0x2000, person at 0x3000 ("PID_IKE"), job at 0x4000 ("JID_RANGER"); the stat bytes follow the
    /// id word of the records.
    fn ram() -> Vec<u8> {
        let mut r = vec![0u8; 0x6000];
        let w = |r: &mut Vec<u8>, a: usize, v: u32| r[a..a + 4].copy_from_slice(&v.to_be_bytes());
        w(&mut r, 0x1000, 0x8000_2000);
        w(&mut r, 0x1004, 0x8000_3000);
        w(&mut r, 0x1008, 0x8000_4000);
        w(&mut r, 0x2010, 0x8000_3000);
        w(&mut r, 0x2014, 0x8000_4000);
        w(&mut r, 0x3000, 0x8000_5000);
        w(&mut r, 0x4000, 0x8000_5100);
        r[0x5000..0x5008].copy_from_slice(b"PID_IKE\0");
        r[0x5100..0x510B].copy_from_slice(b"JID_RANGER\0");
        r
    }

    fn fixture() -> Vec<u8> {
        let mut r = ram();
        r[0x3004] = 1; // person HP
        r[0x4004] = 18; // job HP
        r[0x2020] = 2; // unit gains HP
        r[0x400C] = 40; // job HP cap
        r[0x3005] = 2; // person Str
        r[0x4005] = 3; // job Str
        r[0x400D] = 20; // job Str cap
        r[0x2030] = 21; // current HP
        r
    }

    #[test]
    fn id_names() {
        assert_eq!(id_name(b"PID_IKE\0", "PID_"), "Ike");
        assert_eq!(id_name(b"JID_TIAMAT/F\0", "JID_"), "Tiamat");
        assert_eq!(id_name(b"JID_SOCIAL_KNIGHT\0", "JID_"), "Social Knight");
    }

    #[test]
    fn the_displayed_unit_is_resolved_and_checked() {
        let d = def4();
        let mut r = fixture();
        let s = d.resolve(&r).unwrap();
        assert_eq!((s.unit, s.person, s.job), (0x8000_2000, 0x8000_3000, 0x8000_4000));
        let v = d.view(&r).unwrap();
        assert_eq!((v.name.as_str(), v.class.as_str()), ("Ike", "Ranger"));
        // no sheet shown: pointer 0
        r[0x1000..0x1004].fill(0);
        assert!(d.view(&r).unwrap_err().contains("Open a character sheet"));
        // pointer to a unit whose records disagree with the displayed copy
        let mut r = fixture();
        r[0x1004..0x1008].copy_from_slice(&0x8000_3400u32.to_be_bytes());
        assert!(d.resolve(&r).is_err());
        // a stale object (no id strings behind the records)
        let mut r = fixture();
        r[0x5000] = b'X';
        assert!(d.resolve(&r).is_err());
        // the other unit: the table follows the pointer
        let mut r = fixture();
        r[0x2100..0x2118].fill(0);
        r[0x2110..0x2114].copy_from_slice(&0x8000_3000u32.to_be_bytes());
        r[0x2114..0x2118].copy_from_slice(&0x8000_4000u32.to_be_bytes());
        r[0x1000..0x1004].copy_from_slice(&0x8000_2100u32.to_be_bytes());
        // its unit-relative bytes (gains, current HP) are different: 0
        r[0x2120] = 5;
        let v = d.view(&r).unwrap();
        assert_eq!(v.rows[0].other, Some(5));
        assert_eq!(v.rows[0].total, 24);
    }

    #[test]
    fn breakdown_total_and_range() {
        let d = def4();
        let r = fixture();
        let v = d.view(&r).unwrap();
        let hp = &v.rows[0];
        assert_eq!((hp.base, hp.class, hp.other, hp.total), (1, Some(18), Some(2), 21));
        assert_eq!((hp.cap, hp.min, hp.max), (40, 0, 40 - 18 - 2));
        let s = &v.rows[1];
        assert_eq!((s.base, s.class, s.other, s.total, s.cap, s.max), (2, Some(3), Some(0), 5, 20, 17));
        // current HP: limited by the total of the HP row
        let n = &v.rows[2];
        assert_eq!((n.base, n.class, n.other, n.total, n.min, n.max), (21, None, None, 21, 1, 21));
    }

    #[test]
    fn typed_base_is_clamped_and_only_the_base_is_written() {
        let d = def4();
        let mut r = fixture();
        let before = r.clone();
        let row = d.apply(&mut r, "Str", 10).unwrap();
        assert_eq!((row.base, row.total), (10, 13));
        assert_eq!(r[0x3005], 10);
        // nothing but that byte changed
        let diff: Vec<usize> = (0..r.len()).filter(|i| r[*i] != before[*i]).collect();
        assert_eq!(diff, [0x3005]);
        // above the highest useful base: clamped (cap 20 - class 3 - other 0)
        assert_eq!(d.apply(&mut r, "Str", 1000).unwrap().base, 17);
        assert_eq!(d.apply(&mut r, "Str", 17).unwrap().total, 20);
        // below the minimum
        assert_eq!(d.apply(&mut r, "Str", -5).unwrap().base, 0);
        // current HP cannot pass max HP or go under 1
        assert_eq!(d.apply(&mut r, "HP now", 99).unwrap().base, 21);
        assert_eq!(d.apply(&mut r, "HP now", 0).unwrap().base, 1);
        // a cap byte of 0: the row's own limit
        r[0x400D] = 0;
        assert_eq!(d.view(&r).unwrap().rows[1].cap, 40);
        assert!(d.apply(&mut r, "Nope", 1).is_err());
        // nothing is written when no unit is displayed
        r[0x1000..0x1004].fill(0);
        let snap = r.clone();
        assert!(d.apply(&mut r, "Str", 3).is_err());
        assert_eq!(r, snap);
    }

    #[test]
    fn schema_errors() {
        let bad = |t: &str| compile(toml::from_str(t).unwrap()).unwrap_err();
        assert!(bad("unit = 0x80001000\nperson_at=0\njob_at=4\n").contains("no rows"));
        assert!(bad("unit = 0x90000000\nperson_at=0\njob_at=4\n[[row]]\nlabel=\"a\"\nbase={rec=\"unit\",off=0}\nmax_total=9\n")
            .contains("not in main RAM"));
        assert!(bad("unit = 0x80001000\nperson_at=0\njob_at=4\n[[row]]\nlabel=\"a\"\nbase={rec=\"unit\",off=0}\nmax_total=200\n")
            .contains("signed byte"));
        assert!(bad("unit = 0x80001000\nperson_at=0\njob_at=4\n[[row]]\nlabel=\"a\"\nbase={rec=\"unit\",off=0}\nmax_total=9\nmax_total_of=\"b\"\n")
            .contains("earlier row"));
    }
}
