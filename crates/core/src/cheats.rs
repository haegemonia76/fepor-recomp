//! Cheat codes in Dolphin's game INI format (ADR 0026): `[Gecko]` / `[ActionReplay]` with their `_Enabled`
//! lists, compiled to plain memory writes that the runtime hands to the patch engine (ADR 0025), and the store
//! the overlay and the guest thread share.
//!
//! Only codes that are unconditional writes can be compiled: the patch engine rewrites fixed bytes every frame,
//! it does not run code. Everything else is kept in the list, shown as unsupported with the reason.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Gecko,
    ActionReplay,
    /// An entry of the native TOML cheat files (ADR 0033, `fepor_core::native`).
    Native,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Gecko => "Gecko",
            Kind::ActionReplay => "Action Replay",
            Kind::Native => "Native",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Kind::Gecko => "gecko",
            Kind::ActionReplay => "ar",
            Kind::Native => "native",
        }
    }
}

/// What a cheat does (ADR 0033).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Writes its value when turned on (once, or every frame when `continuous`); turning it off puts back the
    /// bytes it replaced.
    Hold,
    /// "Apply" adds the typed amount to the current value, once, clamped to `min..=max`. Never reverted.
    Add,
    /// "Apply" writes the typed value, once, clamped to `min..=max`. Never reverted.
    Set,
}

impl Action {
    /// Add and Set are one-shot: there is no checkbox and nothing to revert.
    pub fn once(self) -> bool {
        self != Action::Hold
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Hold => "hold",
            Action::Add => "add once",
            Action::Set => "set once",
        }
    }
}

/// The new value of an Add/Set: `current + amount` or `amount`, clamped to `min..=max`.
pub fn amount_result(action: Action, current: u32, amount: i64, min: i64, max: i64) -> u32 {
    let raw = match action {
        Action::Add => current as i64 + amount,
        _ => amount,
    };
    raw.clamp(min, max) as u32
}

/// What an Apply did: the value before and after, and whether the cap or the floor changed the result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applied {
    pub before: u32,
    pub after: u32,
    pub clamped: bool,
}

/// How a [`Condition`] compares the value in RAM with its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A write is made only while the big-endian unsigned value of `size` (1, 2 or 4) bytes at `addr` compares
/// to `value` as `cmp` says. Evaluated by the patch engine when it writes (once per frame for a continuous
/// set), on the RAM of that moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Condition {
    pub addr: u32,
    pub size: u8,
    pub cmp: Cmp,
    pub value: u32,
}

impl Condition {
    /// Whether the condition holds on `ram` (main RAM from physical address 0). An address outside `ram`
    /// does not hold.
    pub fn holds(&self, ram: &[u8]) -> bool {
        let p = (self.addr & 0x3FFF_FFFF) as usize;
        let Some(b) = ram.get(p..p + self.size as usize) else {
            return false;
        };
        let v = b.iter().fold(0u32, |v, b| v << 8 | *b as u32);
        match self.cmp {
            Cmp::Eq => v == self.value,
            Cmp::Ne => v != self.value,
            Cmp::Lt => v < self.value,
            Cmp::Le => v <= self.value,
            Cmp::Gt => v > self.value,
            Cmp::Ge => v >= self.value,
        }
    }
}

/// Bytes to write at a guest address (cached `0x80…` form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    pub addr: u32,
    pub bytes: Vec<u8>,
    /// Only native cheats have one (Gecko and Action Replay writes are unconditional here).
    pub when: Option<Condition>,
}

impl Write {
    pub fn new(addr: u32, bytes: Vec<u8>) -> Write {
        Write { addr, bytes, when: None }
    }
}

/// One code of an INI file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cheat {
    pub kind: Kind,
    pub name: String,
    pub author: String,
    /// `*` comment lines under the header.
    pub notes: Vec<String>,
    /// The address/value pairs as written.
    pub lines: Vec<(u32, u32)>,
    /// The writes the code makes, or why it cannot be run.
    pub writes: Result<Vec<Write>, String>,
    /// Held every frame (Gecko and Action Replay codes always are) or written once when turned on.
    pub continuous: bool,
    /// Hold for everything but the native cheats that ask for Add or Set.
    pub action: Action,
    /// Bounds of an Add/Set result (unused by Hold).
    pub min: i64,
    pub max: i64,
}

impl Cheat {
    /// Add or Set the typed `amount` once on `ram` (main RAM from physical address 0): reads the current
    /// big-endian value of the cheat's single write, computes the result clamped to `min..=max`, writes it.
    /// Nothing is remembered: no later toggle restores anything. Refused when the cheat's condition does not
    /// hold, or the cheat is a Hold one.
    pub fn apply(&self, amount: i64, ram: &mut [u8]) -> Result<Applied, String> {
        if !self.action.once() {
            return Err("this cheat is held while on, not applied".into());
        }
        let w = match self.writes.as_ref().map(|w| w.as_slice()) {
            Ok([w]) => w,
            Ok(_) => return Err("the cheat needs exactly one write".into()),
            Err(e) => return Err(e.clone()),
        };
        if w.when.is_some_and(|c| !c.holds(ram)) {
            return Err("the game is not in the state this cheat needs (its condition does not hold)".into());
        }
        let p = (w.addr & 0x3FFF_FFFF) as usize;
        let cell = ram.get_mut(p..p + w.bytes.len()).ok_or("address outside main RAM")?;
        let before = cell.iter().fold(0u32, |v, b| v << 8 | *b as u32);
        let after = amount_result(self.action, before, amount, self.min, self.max);
        let raw = match self.action {
            Action::Add => before as i64 + amount,
            _ => amount,
        };
        let n = cell.len();
        cell.copy_from_slice(&after.to_be_bytes()[4 - n..]);
        Ok(Applied { before, after, clamped: raw != after as i64 })
    }

    /// The value an Add/Set cheat would change now (`None`: its condition does not hold, or it is a held cheat).
    pub fn current(&self, ram: &[u8]) -> Option<u32> {
        if !self.action.once() {
            return None;
        }
        let w = self.writes.as_ref().ok()?.first()?;
        if w.when.is_some_and(|c| !c.holds(ram)) {
            return None;
        }
        let p = (w.addr & 0x3FFF_FFFF) as usize;
        ram.get(p..p + w.bytes.len()).map(|b| b.iter().fold(0u32, |v, b| v << 8 | *b as u32))
    }

    /// Identity used in the enabled lists (`gecko:Name`, `ar:Name`).
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind.tag(), self.name)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ini {
    pub cheats: Vec<Cheat>,
    /// Keys listed in `[Gecko_Enabled]` / `[ActionReplay_Enabled]`.
    pub enabled: Vec<String>,
}

/// Prefix of the request key of a character stat (never a cheat key: those start with the kind tag).
const CHAR_KEY: &str = "char:";

const RAM_MAX_WRITE: usize = 24 << 20;

fn address(word: u32) -> u32 {
    0x8000_0000 | (word & 0x01FF_FFFF)
}

fn repeat(unit: &[u8], count: u32) -> Result<Vec<u8>, String> {
    let total = unit.len() as u64 * count as u64;
    if total > RAM_MAX_WRITE as u64 {
        return Err(format!("repeat count {count} is larger than main RAM"));
    }
    Ok(unit.repeat(count as usize))
}

/// Gecko code types 00 (8-bit), 02 (16-bit), 04 (32-bit) and 06 (string) with the 0x80 base address.
pub fn compile_gecko(lines: &[(u32, u32)]) -> Result<Vec<Write>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let (a, d) = lines[i];
        i += 1;
        let kind = a >> 24;
        if a >> 29 != 0 {
            return Err(format!(
                "code type {kind:02X} is not supported (conditionals, loops, jumps, assembly insertion, base address)"
            ));
        }
        if a & 0x1000_0000 != 0 {
            return Err(format!(
                "code type {kind:02X}: pointer-relative addressing is not supported"
            ));
        }
        let bytes = match (a >> 25) & 7 {
            0 => repeat(&[d as u8], (d >> 8) + 1)?,
            1 => repeat(&(d as u16).to_be_bytes(), (d >> 16) + 1)?,
            2 => d.to_be_bytes().to_vec(),
            3 => {
                let len = d as usize;
                let n = len.div_ceil(8);
                let data = lines
                    .get(i..i + n)
                    .ok_or_else(|| "string write is cut short".to_string())?;
                i += n;
                let mut b: Vec<u8> = data
                    .iter()
                    .flat_map(|(x, y)| x.to_be_bytes().into_iter().chain(y.to_be_bytes()))
                    .collect();
                b.truncate(len);
                b
            }
            _ => {
                return Err(format!(
                    "code type {kind:02X} (serial write) is not supported"
                ))
            }
        };
        out.push(Write::new(address(a), bytes));
    }
    Ok(out)
}

/// Action Replay normal codes: RAM write and fill (8, 16 and 32 bits). Conditionals, pointer writes, add codes
/// and master codes are not supported.
pub fn compile_ar(lines: &[(u32, u32)]) -> Result<Vec<Write>, String> {
    let mut out = Vec::new();
    for &(a, d) in lines {
        if a >> 27 != 0 {
            return Err(format!(
                "code type {:02X} is not supported (conditionals, pointer writes, add codes, master codes)",
                a >> 24
            ));
        }
        let bytes = match (a >> 25) & 3 {
            0 => repeat(&[d as u8], (d >> 8) + 1)?,
            1 => repeat(&(d as u16).to_be_bytes(), (d >> 16) + 1)?,
            2 => d.to_be_bytes().to_vec(),
            _ => return Err("fill-and-slide write is not supported".into()),
        };
        out.push(Write::new(address(a), bytes));
    }
    Ok(out)
}

fn compile(kind: Kind, lines: &[(u32, u32)]) -> Result<Vec<Write>, String> {
    if lines.is_empty() {
        return Err("the code has no lines".into());
    }
    match kind {
        Kind::Gecko => compile_gecko(lines),
        Kind::ActionReplay => compile_ar(lines),
        Kind::Native => Err("native cheats are not compiled from hex lines".into()),
    }
}

fn hex_line(l: &str) -> Option<(u32, u32)> {
    let mut it = l.split_whitespace();
    let (a, d) = (it.next()?, it.next()?);
    if it.next().is_some() || a.len() != 8 || d.len() != 8 {
        return None;
    }
    Some((
        u32::from_str_radix(a, 16).ok()?,
        u32::from_str_radix(d, 16).ok()?,
    ))
}

/// `Name [Author]` header text after the `$`.
fn split_author(h: &str) -> (String, String) {
    let h = h.trim();
    if let Some(body) = h.strip_suffix(']') {
        if let Some(i) = body.rfind(" [") {
            return (
                body[..i].trim().to_string(),
                body[i + 2..].trim().to_string(),
            );
        }
    }
    (h.to_string(), String::new())
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    None,
    Codes(Kind),
    Enabled(Kind),
}

/// Parse the cheat sections of a game INI; other sections (`[Core]`, `[OnFrame]`, ...) are skipped. A line that
/// is neither a header, a comment nor `XXXXXXXX XXXXXXXX` makes the code it belongs to unusable.
pub fn parse_ini(text: &str) -> Ini {
    let mut ini = Ini::default();
    let mut section = Section::None;
    let mut bad: Option<String> = None;
    let mut current: Option<Cheat> = None;
    fn finish(ini: &mut Ini, cur: &mut Option<Cheat>, bad: &mut Option<String>) {
        if let Some(mut c) = cur.take() {
            c.writes = match bad.take() {
                Some(e) => Err(e),
                None => compile(c.kind, &c.lines),
            };
            ini.cheats.push(c);
        }
        *bad = None;
    }
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.split(']').next()) {
            finish(&mut ini, &mut current, &mut bad);
            section = match name.to_ascii_lowercase().as_str() {
                "gecko" => Section::Codes(Kind::Gecko),
                "actionreplay" => Section::Codes(Kind::ActionReplay),
                "gecko_enabled" => Section::Enabled(Kind::Gecko),
                "actionreplay_enabled" => Section::Enabled(Kind::ActionReplay),
                _ => Section::None,
            };
            continue;
        }
        match section {
            Section::None => {}
            Section::Enabled(kind) => {
                if let Some(h) = line.strip_prefix('$') {
                    ini.enabled
                        .push(format!("{}:{}", kind.tag(), split_author(h).0));
                }
            }
            Section::Codes(kind) => {
                if let Some(h) = line.strip_prefix('$').or_else(|| line.strip_prefix('+')) {
                    finish(&mut ini, &mut current, &mut bad);
                    let (name, author) = split_author(h);
                    current = Some(Cheat {
                        kind,
                        name,
                        author,
                        notes: Vec::new(),
                        lines: Vec::new(),
                        writes: Err(String::new()),
                        continuous: true,
                        action: Action::Hold,
                        min: 0,
                        max: 0,
                    });
                } else if let Some(n) = line.strip_prefix('*') {
                    if let Some(c) = &mut current {
                        c.notes.push(n.trim().to_string());
                    }
                } else if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                } else if let Some(c) = &mut current {
                    match hex_line(line) {
                        Some(l) => c.lines.push(l),
                        None => bad = Some(format!("unreadable line {line:?}")),
                    }
                }
            }
        }
    }
    finish(&mut ini, &mut current, &mut bad);
    ini
}

/// A code as the overlay shows it.
#[derive(Clone, Debug)]
pub struct CheatView {
    pub key: String,
    pub name: String,
    pub author: String,
    pub kind: Kind,
    pub action: Action,
    pub min: i64,
    pub max: i64,
    pub notes: Vec<String>,
    pub enabled: bool,
    /// `Err(reason)`: the code cannot run and its checkbox is disabled.
    pub supported: Result<(), String>,
    /// Why the runtime could not apply an enabled code (overlap with a mod, ...).
    pub status: Option<String>,
    /// What the last Apply of an Add/Set cheat did (`Err`: why it did not).
    pub result: Option<Result<String, String>>,
    /// The value an Add/Set cheat acts on, read by the guest thread a few times a second.
    pub current: Option<u32>,
}

struct Entry {
    cheat: Cheat,
    enabled: bool,
    status: Option<String>,
    result: Option<Result<String, String>>,
    current: Option<u32>,
}

#[derive(Default)]
struct Inner {
    game: Option<String>,
    /// Guest address ranges of the game's code: recompiled, so writes there change nothing (ADR 0025).
    code: Vec<std::ops::Range<u32>>,
    /// The native cheats of the game (shipped and the player's file), kept to rebuild the list on an import.
    native: Vec<Cheat>,
    native_error: Option<String>,
    entries: Vec<Entry>,
    /// Apply clicks waiting for the guest thread: (key, amount).
    requests: Vec<(String, i64)>,
    /// The character table of the game and what the displayed unit looks like (ADR 0033, `crate::character`).
    character: Option<crate::character::CharDef>,
    character_view: Option<Result<crate::character::CharView, String>>,
    ticks: u32,
}

/// `<data>/cheats.toml`: the enabled codes per game ID.
#[derive(Default, Serialize, Deserialize)]
struct SavedFile {
    #[serde(default)]
    enabled: BTreeMap<String, Vec<String>>,
}

/// The cheat list of the running game. The overlay (render thread) edits it, the guest thread applies it: it
/// watches [`CheatStore::generation`] and rebuilds its patch sets when it moved.
pub struct CheatStore {
    data: PathBuf,
    inner: Mutex<Inner>,
    generation: AtomicU64,
    /// Set when `Inner::requests` is not empty.
    pending: AtomicBool,
}

fn overlaps_code(w: &Write, code: &[std::ops::Range<u32>]) -> bool {
    let (s, e) = (
        w.addr & 0x01FF_FFFF,
        (w.addr & 0x01FF_FFFF) as u64 + w.bytes.len() as u64,
    );
    code.iter()
        .any(|r| (s as u64) < r.end as u64 && e > r.start as u64)
}

/// A code the guest thread has to apply.
#[derive(Clone, Debug)]
pub struct Wanted {
    pub key: String,
    pub name: String,
    pub writes: Vec<Write>,
    /// Held every frame, or written once when turned on.
    pub continuous: bool,
}

impl CheatStore {
    pub fn new(data: PathBuf) -> CheatStore {
        CheatStore {
            data,
            inner: Mutex::default(),
            generation: AtomicU64::new(0),
            pending: AtomicBool::new(false),
        }
    }

    fn ini_path(&self, game: &str) -> PathBuf {
        self.data.join("cheats").join(format!("{game}.ini"))
    }

    fn saved_path(&self) -> PathBuf {
        self.data.join("cheats.toml")
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn game(&self) -> Option<String> {
        self.lock().game.clone()
    }

    fn load_saved(&self) -> SavedFile {
        std::fs::read_to_string(self.saved_path())
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, inner: &Inner) {
        let Some(game) = &inner.game else { return };
        let mut file = self.load_saved();
        let on: Vec<String> = inner
            .entries
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.cheat.key())
            .collect();
        file.enabled.insert(game.clone(), on);
        if let Ok(text) = toml::to_string(&file) {
            let _ = std::fs::create_dir_all(&self.data);
            if let Err(e) = std::fs::write(self.saved_path(), text) {
                eprintln!("cheats: cannot save {}: {e}", self.saved_path().display());
            }
        }
    }

    fn fill(inner: &mut Inner, ini: Ini, enabled: &[String]) {
        let mut cheats = inner.native.clone();
        cheats.extend(ini.cheats);
        for c in &mut cheats {
            if let Ok(w) = &c.writes {
                if w.iter().any(|w| overlaps_code(w, &inner.code)) {
                    c.writes = Err(
                        "writes into the game's code, which is recompiled: it would change nothing"
                            .into(),
                    );
                }
            }
        }
        inner.entries = cheats
            .into_iter()
            .map(|cheat| Entry {
                enabled: enabled.contains(&cheat.key()) && cheat.writes.is_ok() && !cheat.action.once(),
                cheat,
                status: None,
                result: None,
                current: None,
            })
            .collect();
    }

    /// Select the running game: loads the native cheats (shipped and `<data>/cheats/<id>.toml`, ADR 0033),
    /// `<data>/cheats/<id>.ini` and the enabled list of `<data>/cheats.toml` (the INI's own `_Enabled` lists the
    /// first time). `code` are the guest address ranges of the game's code.
    pub fn set_game(&self, game: &str, code: Vec<std::ops::Range<u32>>) {
        let ini = std::fs::read_to_string(self.ini_path(game))
            .map(|t| parse_ini(&t))
            .unwrap_or_default();
        let enabled = self
            .load_saved()
            .enabled
            .remove(game)
            .unwrap_or_else(|| ini.enabled.clone());
        let (native, native_error) = crate::native::load(game, &self.data.join("cheats").join(format!("{game}.toml")));
        let (character, _) = crate::native::load_character(game, &self.data.join("cheats").join(format!("{game}.toml")));
        let mut inner = self.lock();
        inner.character = character;
        inner.character_view = None;
        inner.game = Some(game.into());
        inner.code = code;
        inner.native = native;
        inner.native_error = native_error;
        Self::fill(&mut inner, ini, &enabled);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Why `<data>/cheats/<id>.toml` could not be read, if it could not (the shipped cheats are listed anyway).
    pub fn native_error(&self) -> Option<String> {
        self.lock().native_error.clone()
    }

    pub fn list(&self) -> Vec<CheatView> {
        self.lock()
            .entries
            .iter()
            .map(|e| CheatView {
                key: e.cheat.key(),
                name: e.cheat.name.clone(),
                author: e.cheat.author.clone(),
                kind: e.cheat.kind,
                action: e.cheat.action,
                min: e.cheat.min,
                max: e.cheat.max,
                notes: e.cheat.notes.clone(),
                enabled: e.enabled,
                supported: e.cheat.writes.as_ref().map(|_| ()).map_err(|s| s.clone()),
                status: e.status.clone(),
                result: e.result.clone(),
                current: e.current,
            })
            .collect()
    }

    /// Turn a code on or off and save the choice. Unsupported codes stay off.
    pub fn set_enabled(&self, key: &str, on: bool) {
        let mut inner = self.lock();
        let Some(e) = inner.entries.iter_mut().find(|e| e.cheat.key() == key) else {
            return;
        };
        if e.enabled == on || (on && (e.cheat.writes.is_err() || e.cheat.action.once())) {
            return;
        }
        e.enabled = on;
        e.status = None;
        self.save(&inner);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Read a Dolphin INI chosen by the player, keep a copy as `<data>/cheats/<game>.ini` and replace the list.
    /// Codes that were on stay on, and so do those the file lists as enabled. Returns the number of codes.
    pub fn import(&self, file: &Path) -> Result<usize, String> {
        let game = self.game().ok_or("no game is running")?;
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let ini = parse_ini(&text);
        if ini.cheats.is_empty() {
            return Err("the file has no [Gecko] or [ActionReplay] codes".into());
        }
        let dest = self.ini_path(&game);
        std::fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))
            .map_err(|e| e.to_string())?;
        std::fs::write(&dest, &text)
            .map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
        let n = ini.cheats.len();
        let mut inner = self.lock();
        let mut enabled: Vec<String> = inner
            .entries
            .iter()
            .filter(|e| e.enabled)
            .map(|e| e.cheat.key())
            .collect();
        enabled.extend(ini.enabled.iter().cloned());
        Self::fill(&mut inner, ini, &enabled);
        self.save(&inner);
        self.generation.fetch_add(1, Ordering::AcqRel);
        Ok(n)
    }

    /// What the guest thread has to apply: the enabled, runnable codes.
    pub fn wanted(&self) -> Vec<Wanted> {
        self.lock()
            .entries
            .iter()
            .filter(|e| e.enabled && !e.cheat.action.once())
            .filter_map(|e| {
                Some(Wanted {
                    key: e.cheat.key(),
                    name: e.cheat.name.clone(),
                    writes: e.cheat.writes.clone().ok()?,
                    continuous: e.cheat.continuous,
                })
            })
            .collect()
    }

    /// The player clicked Apply on an Add/Set cheat with `amount`; the guest thread does it on its next frame
    /// ([`CheatStore::take_requests`]). Does not move the generation: nothing is held.
    pub fn request(&self, key: &str, amount: i64) {
        let mut inner = self.lock();
        if inner.entries.iter().any(|e| e.cheat.key() == key && e.cheat.action.once()) {
            inner.requests.push((key.to_string(), amount));
            self.pending.store(true, Ordering::Release);
        }
    }

    /// The player clicked Apply on a row of the character table: write `typed` as that stat's base for the unit
    /// the game displays, on the guest thread's next frame.
    pub fn request_stat(&self, label: &str, typed: i64) {
        let mut inner = self.lock();
        if inner.character.is_some() {
            inner.requests.push((format!("{CHAR_KEY}{label}"), typed));
            self.pending.store(true, Ordering::Release);
        }
    }

    /// The displayed character as the guest thread last read it (`None`: the game has no character table;
    /// `Err`: no sheet is shown).
    pub fn character(&self) -> Option<Result<crate::character::CharView, String>> {
        let inner = self.lock();
        inner.character.as_ref()?;
        Some(inner.character_view.clone().unwrap_or_else(|| Err("Open a character sheet (Y on the map)".into())))
    }

    /// Once per frame on the guest thread: apply the waiting Add/Set and stat requests on `ram`, record what each
    /// did, and a few times a second refresh what the page shows (current values, the displayed character).
    pub fn run_requests(&self, ram: &mut [u8]) {
        let pending = self.pending.swap(false, Ordering::AcqRel);
        let mut inner = self.lock();
        inner.ticks = inner.ticks.wrapping_add(1);
        if !pending && !inner.ticks.is_multiple_of(6) {
            return;
        }
        for (key, amount) in std::mem::take(&mut inner.requests) {
            if let Some(label) = key.strip_prefix(CHAR_KEY) {
                if let Some(def) = &inner.character {
                    if let Err(e) = def.apply(ram, label, amount) {
                        eprintln!("cheats: stat {label}: {e}");
                    }
                }
                continue;
            }
            let Some(e) = inner.entries.iter_mut().find(|e| e.cheat.key() == key) else {
                continue;
            };
            e.result = Some(e.cheat.apply(amount, ram).map(|a| {
                let note = if a.clamped {
                    format!(" (limited to {}..{})", e.cheat.min, e.cheat.max)
                } else {
                    String::new()
                };
                format!("{} -> {}{note}", a.before, a.after)
            }));
        }
        for e in inner.entries.iter_mut().filter(|e| e.cheat.action.once()) {
            e.current = e.cheat.current(ram);
        }
        inner.character_view = inner.character.as_ref().map(|d| d.view(ram));
    }

    /// The guest thread reports why a wanted code is not applied (`None`: it is). Does not move the generation.
    pub fn set_status(&self, key: &str, status: Option<String>) {
        if let Some(e) = self
            .lock()
            .entries
            .iter_mut()
            .find(|e| e.cheat.key() == key)
        {
            e.status = status;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INI: &str = "\
[Core]
CPUCore = 1
[Gecko]
$Infinite Money [Someone]
*Gold stays at 99999
0442A000 0001869F
$Fill 16
02001000 0002BEEF
$Text
06001100 0000000B
48656C6C 6F20776F
726C6400 00000000
$Needs a condition
20420000 00000001
04001000 00000001
E0000000 80008000
$Bad line
0400 12
[Gecko_Enabled]
$Infinite Money
[ActionReplay]
$AR Write
04002000 12345678
00002010 00000300
$AR Conditional
C8002000 12345678
[ActionReplay_Enabled]
$AR Write
[OnFrame]
$Ignored
04000000 00000000
";

    fn find<'a>(i: &'a Ini, n: &str) -> &'a Cheat {
        i.cheats
            .iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("{n}"))
    }

    #[test]
    fn sections_names_authors_and_enabled_lists() {
        let ini = parse_ini(INI);
        assert_eq!(ini.cheats.len(), 7);
        let m = find(&ini, "Infinite Money");
        assert_eq!(m.author, "Someone");
        assert_eq!(m.notes, ["Gold stays at 99999"]);
        assert_eq!(ini.enabled, ["gecko:Infinite Money", "ar:AR Write"]);
        assert_eq!(m.key(), "gecko:Infinite Money");
    }

    #[test]
    fn gecko_writes() {
        let ini = parse_ini(INI);
        assert_eq!(
            find(&ini, "Infinite Money").writes,
            Ok(vec![Write::new(0x8042_A000, vec![0, 1, 0x86, 0x9F])])
        );
        // 16-bit: count = 2 + 1 of BEEF
        assert_eq!(
            find(&ini, "Fill 16").writes,
            Ok(vec![Write::new(0x8000_1000, [0xBE, 0xEF].repeat(3))])
        );
        // string of 11 bytes over two data lines, the tail of the last line is cut
        assert_eq!(
            find(&ini, "Text").writes,
            Ok(vec![Write::new(0x8000_1100, b"Hello world".to_vec())])
        );
    }

    #[test]
    fn unsupported_and_broken_codes_say_why() {
        let ini = parse_ini(INI);
        let e = find(&ini, "Needs a condition").writes.clone().unwrap_err();
        assert!(e.contains("20"), "{e}");
        assert!(find(&ini, "Bad line")
            .writes
            .clone()
            .unwrap_err()
            .contains("unreadable"));
        assert!(find(&ini, "AR Conditional")
            .writes
            .clone()
            .unwrap_err()
            .contains("C8"));
    }

    #[test]
    fn action_replay_writes() {
        let ini = parse_ini(INI);
        assert_eq!(
            find(&ini, "AR Write").writes,
            Ok(vec![
                Write::new(0x8000_2000, vec![0x12, 0x34, 0x56, 0x78]),
                Write::new(0x8000_2010, vec![0; 4]),
            ])
        );
    }

    #[test]
    fn upper_cache_bit_and_pointer_flag() {
        assert_eq!(
            compile_gecko(&[(0x0501_0000, 7)]).unwrap(),
            vec![Write::new(0x8101_0000, vec![0, 0, 0, 7])]
        );
        assert!(compile_gecko(&[(0x1400_0000, 7)])
            .unwrap_err()
            .contains("pointer"));
        assert!(compile_gecko(&[(0x0800_0000, 7), (0, 0)])
            .unwrap_err()
            .contains("serial"));
    }

    #[test]
    fn store_persists_the_enabled_codes_and_imports() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("in.ini");
        std::fs::write(&file, INI).unwrap();
        let data = dir.path().join("data");
        let s = CheatStore::new(data.clone());
        assert!(s.import(&file).is_err(), "no game yet");
        // the second code range overlaps nothing, the first one covers 0x8000_1000
        s.set_game("GFEP01", vec![0x1000..0x1004]);
        assert_eq!(s.import(&file).unwrap(), 7);
        let list = s.list();
        // enabled by the file's own list
        assert!(
            list.iter()
                .find(|c| c.name == "Infinite Money")
                .unwrap()
                .enabled
        );
        assert!(list.iter().find(|c| c.name == "AR Write").unwrap().enabled);
        // `Fill 16` writes over the code range: unsupported
        assert!(list
            .iter()
            .find(|c| c.name == "Fill 16")
            .unwrap()
            .supported
            .is_err());
        let g = s.generation();
        s.set_enabled("gecko:Infinite Money", false);
        s.set_enabled("gecko:Fill 16", true);
        assert!(s.generation() > g);
        assert_eq!(s.wanted().len(), 1);
        // a new store reads the choice back, not the INI's list
        let again = CheatStore::new(data);
        again.set_game("GFEP01", vec![0x1000..0x1004]);
        let l = again.list();
        assert!(
            !l.iter()
                .find(|c| c.name == "Infinite Money")
                .unwrap()
                .enabled
        );
        assert!(l.iter().find(|c| c.name == "AR Write").unwrap().enabled);
        assert_eq!(again.wanted().len(), 1);
    }
}
