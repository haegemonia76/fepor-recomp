//! Memory patch engine (ADR 0025): sets of `(address, bytes)` written over guest RAM by their owners (cheats,
//! mods, tools), with conflict detection between sources and a hook that runs once per video frame.
//!
//! Patches only touch data. Recompiled code is not in guest RAM (ADR 0001), so a patch over a function's
//! instructions changes nothing in either backend.

use std::collections::HashMap;

use fepor_core::cheats::Condition;
use fepor_core::version;

use crate::machine::RAM_SIZE;

/// Handle of a registered set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SetId(u32);

/// Bytes to write at a guest address (cached `0x80…` or uncached `0xC0…` mirror, or physical).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub addr: u32,
    pub bytes: Vec<u8>,
    /// Written only while it holds (checked at every write, on the RAM of that moment).
    pub when: Option<Condition>,
}

impl Patch {
    pub fn new(addr: u32, bytes: impl Into<Vec<u8>>) -> Patch {
        Patch {
            addr,
            bytes: bytes.into(),
            when: None,
        }
    }

    /// The same patch, written only while `when` holds.
    pub fn when(mut self, when: Option<Condition>) -> Patch {
        self.when = when;
        self
    }

    /// Write the bytes unless the condition says no.
    fn apply(&self, ram: &mut [u8]) {
        if self.when.is_none_or(|c| c.holds(ram)) {
            ram[self.range()].copy_from_slice(&self.bytes);
        }
    }

    /// Big-endian 32-bit store.
    pub fn word(addr: u32, value: u32) -> Patch {
        Patch::new(addr, value.to_be_bytes())
    }

    fn range(&self) -> std::ops::Range<usize> {
        let p = phys(self.addr);
        p..p + self.bytes.len()
    }
}

/// What a source registers: patches for one game, written once when enabled, or every frame when `continuous`
/// (a cheat that holds a value; the game's own writes are overwritten at the next frame).
#[derive(Clone, Debug)]
pub struct PatchSet {
    /// Who owns the set (`"cheat:Infinite HP"`, `"mod:foo"`). Sets of one source may overlap, sets of different
    /// sources may not be enabled together.
    pub source: String,
    /// Game ID the addresses belong to (`version::lookup`).
    pub game_id: String,
    pub patches: Vec<Patch>,
    pub continuous: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PatchError {
    #[error("no game selected for the patch engine")]
    NoGame,
    #[error("unsupported game ID {0:?}")]
    UnknownGame(String),
    #[error("patch set is for {set}, the running game is {running}")]
    WrongGame { set: String, running: String },
    #[error("patch at {addr:#010x} (+{len} bytes) is not inside main RAM")]
    OutOfRange { addr: u32, len: usize },
    #[error("no such patch set")]
    NoSuchSet,
    #[error("overlaps an enabled set of another source: {0:?}")]
    Conflict(Vec<Conflict>),
}

/// Two sources writing the same bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// The enabled set already owning the bytes.
    pub with: SetId,
    pub with_source: String,
    /// First overlapping physical address and the length of the overlap's first run.
    pub addr: u32,
    pub len: usize,
}

struct Entry {
    set: PatchSet,
    /// Present while enabled: the bytes each patch replaced (taken when enabled), in patch order.
    saved: Option<Vec<Vec<u8>>>,
}

#[derive(Default)]
pub struct PatchEngine {
    game: Option<String>,
    sets: HashMap<SetId, Entry>,
    next: u32,
    /// Frames the hook has run for.
    pub frames: u64,
    /// Video retraces seen at the last hook run (several retraces between two runs make one frame).
    last_retrace: u64,
    pub trace: bool,
    /// `FEPOR_DUMP_RAM=<dir>[:<every>[:<from>[:<to>]]]`: main RAM written to `<dir>/ram_<frame>.bin` every `<every>`
    /// hook frames (default 120) from frame `<from>` to `<to>` (default: always), for offline searches
    /// (tools/README.md).
    dump: Option<RamDump>,
    /// The memory watcher's guest-thread side, served from `frame` (ADR 0048).
    watcher: Option<crate::watch::Watcher>,
}

/// What `FEPOR_DUMP_RAM` asks for.
struct RamDump {
    dir: std::path::PathBuf,
    every: u64,
    from: u64,
    to: u64,
}

fn parse_dump(v: &str) -> RamDump {
    // the numbers are taken from the right: a directory name may contain ':'
    let mut parts: Vec<u64> = Vec::new();
    let mut dir = v;
    while parts.len() < 3 {
        match dir.rsplit_once(':') {
            Some((d, n)) if n.parse::<u64>().is_ok() => {
                parts.push(n.parse().unwrap());
                dir = d;
            }
            _ => break,
        }
    }
    parts.reverse();
    RamDump {
        dir: dir.into(),
        every: parts.first().copied().unwrap_or(120).max(1),
        from: parts.get(1).copied().unwrap_or(0),
        to: parts.get(2).copied().unwrap_or(u64::MAX),
    }
}

fn phys(ea: u32) -> usize {
    (ea & 0x3FFF_FFFF) as usize
}

impl PatchEngine {
    pub fn new() -> PatchEngine {
        PatchEngine {
            trace: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "patch" || x == "all")),
            dump: std::env::var("FEPOR_DUMP_RAM").ok().map(|v| parse_dump(&v)),
            ..Default::default()
        }
    }

    /// Select the running game; sets for another game are refused. Disabled and enabled sets stay as they are.
    pub fn set_game(&mut self, game_id: &str) -> Result<(), PatchError> {
        version::lookup(game_id).ok_or_else(|| PatchError::UnknownGame(game_id.into()))?;
        self.game = Some(game_id.into());
        Ok(())
    }

    /// Serve the memory watcher's requests from the per-frame hook.
    pub fn attach_watcher(&mut self, link: std::sync::Arc<fepor_core::memwatch::Link>) {
        self.watcher = Some(crate::watch::Watcher::new(link));
    }

    pub fn game(&self) -> Option<&str> {
        self.game.as_deref()
    }

    /// Add a set (disabled). Validates the game ID and the address ranges.
    pub fn register(&mut self, set: PatchSet) -> Result<SetId, PatchError> {
        let running = self.game.as_deref().ok_or(PatchError::NoGame)?;
        if set.game_id != running {
            return Err(PatchError::WrongGame {
                set: set.game_id,
                running: running.into(),
            });
        }
        for p in &set.patches {
            let r = p.range();
            if r.end > RAM_SIZE || phys(p.addr) >= RAM_SIZE {
                return Err(PatchError::OutOfRange {
                    addr: p.addr,
                    len: p.bytes.len(),
                });
            }
        }
        let id = SetId(self.next);
        self.next += 1;
        self.sets.insert(id, Entry { set, saved: None });
        Ok(id)
    }

    /// Drop a set, reverting it first when enabled.
    pub fn unregister(&mut self, id: SetId, ram: &mut [u8]) -> Result<PatchSet, PatchError> {
        self.disable(id, ram)?;
        Ok(self.sets.remove(&id).ok_or(PatchError::NoSuchSet)?.set)
    }

    pub fn is_enabled(&self, id: SetId) -> bool {
        self.sets.get(&id).is_some_and(|e| e.saved.is_some())
    }

    /// Overlaps of a set with the enabled sets of other sources (empty: it can be enabled).
    pub fn conflicts(&self, id: SetId) -> Result<Vec<Conflict>, PatchError> {
        let mine = &self.sets.get(&id).ok_or(PatchError::NoSuchSet)?.set;
        let mut out = Vec::new();
        for (&other, e) in &self.sets {
            if other == id || e.saved.is_none() || e.set.source == mine.source {
                continue;
            }
            for a in &mine.patches {
                for b in &e.set.patches {
                    let (ra, rb) = (a.range(), b.range());
                    let start = ra.start.max(rb.start);
                    let end = ra.end.min(rb.end);
                    if start < end {
                        out.push(Conflict {
                            with: other,
                            with_source: e.set.source.clone(),
                            addr: start as u32,
                            len: end - start,
                        });
                    }
                }
            }
        }
        out.sort_by_key(|c| (c.addr, c.with.0));
        Ok(out)
    }

    /// Write the set's patches, remembering the bytes they replace. Refused while another source's enabled set
    /// writes the same bytes. Enabling an enabled set is a no-op.
    pub fn enable(&mut self, id: SetId, ram: &mut [u8]) -> Result<(), PatchError> {
        if self.is_enabled(id) {
            return Ok(());
        }
        let c = self.conflicts(id)?;
        if !c.is_empty() {
            return Err(PatchError::Conflict(c));
        }
        let e = self.sets.get_mut(&id).ok_or(PatchError::NoSuchSet)?;
        let mut saved = Vec::with_capacity(e.set.patches.len());
        for p in &e.set.patches {
            saved.push(ram[p.range()].to_vec());
            p.apply(ram);
        }
        if self.trace {
            eprintln!(
                "patch: enabled {:?} ({} patches)",
                e.set.source,
                saved.len()
            );
        }
        e.saved = Some(saved);
        Ok(())
    }

    /// Put back the bytes the set replaced. Disabling a disabled set is a no-op. A value the game wrote since
    /// the set was enabled is lost: the bytes of the moment of enabling return.
    pub fn disable(&mut self, id: SetId, ram: &mut [u8]) -> Result<(), PatchError> {
        let e = self.sets.get_mut(&id).ok_or(PatchError::NoSuchSet)?;
        if let Some(saved) = e.saved.take() {
            // reverse order: patches of one set may overlap
            for (p, old) in e.set.patches.iter().zip(&saved).rev() {
                ram[p.range()].copy_from_slice(old);
            }
            if self.trace {
                eprintln!("patch: disabled {:?}", e.set.source);
            }
        }
        Ok(())
    }

    /// Write every enabled set again (after the guest's memory was replaced, e.g. a save state load, or when the
    /// game rewrote data a one-shot patch changed). The originals kept at enabling stay valid.
    pub fn reapply(&self, ram: &mut [u8]) {
        for e in self.sets.values().filter(|e| e.saved.is_some()) {
            for p in &e.set.patches {
                p.apply(ram);
            }
        }
    }

    /// The once-per-frame hook, called on the guest thread with the VI retrace count: writes the continuous sets
    /// again when a new frame began. Returns whether it ran.
    pub fn frame(&mut self, retraces: u64, ram: &mut [u8]) -> bool {
        if retraces == self.last_retrace {
            return false;
        }
        self.last_retrace = retraces;
        self.frames += 1;
        let mut active = 0;
        for e in self.sets.values().filter(|e| e.saved.is_some()) {
            active += 1;
            if e.set.continuous {
                for p in &e.set.patches {
                    p.apply(ram);
                }
            }
        }
        if self.trace && self.frames.is_multiple_of(60) {
            eprintln!(
                "patch: frame hook #{} (retrace {retraces}), {active} enabled sets",
                self.frames
            );
        }
        if let Some(d) = &self.dump {
            if self.frames.is_multiple_of(d.every) && (d.from..=d.to).contains(&self.frames) {
                let _ = std::fs::create_dir_all(&d.dir);
                let file = d.dir.join(format!("ram_{:06}.bin", self.frames));
                if let Err(e) = std::fs::write(&file, &ram[..RAM_SIZE]) {
                    eprintln!("patch: cannot write {}: {e}", file.display());
                }
            }
        }
        if let Some(mut w) = self.watcher.take() {
            w.service(self, ram);
            self.watcher = Some(w);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> PatchEngine {
        let mut e = PatchEngine::default();
        e.set_game("GFEP01").unwrap();
        e
    }

    fn set(source: &str, patches: Vec<Patch>, continuous: bool) -> PatchSet {
        PatchSet {
            source: source.into(),
            game_id: "GFEP01".into(),
            patches,
            continuous,
        }
    }

    fn ram() -> Vec<u8> {
        (0..RAM_SIZE + 64).map(|i| i as u8).collect()
    }

    #[test]
    fn apply_and_revert() {
        let mut e = engine();
        let mut r = ram();
        let before = r[0x100..0x108].to_vec();
        let id = e
            .register(set("a", vec![Patch::word(0x8000_0100, 0xDEADBEEF)], false))
            .unwrap();
        assert_eq!(r[0x100..0x108], before[..]);
        e.enable(id, &mut r).unwrap();
        assert_eq!(r[0x100..0x104], [0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(e.is_enabled(id));
        e.disable(id, &mut r).unwrap();
        assert_eq!(r[0x100..0x108], before[..]);
        e.enable(id, &mut r).unwrap();
        e.unregister(id, &mut r).unwrap();
        assert_eq!(r[0x100..0x108], before[..]);
    }

    #[test]
    fn overlapping_patches_of_one_set_revert_fully() {
        let mut e = engine();
        let mut r = ram();
        let before = r[0x200..0x208].to_vec();
        let id = e
            .register(set(
                "a",
                vec![
                    Patch::new(0x200, [1, 1, 1, 1]),
                    Patch::new(0x802, [2, 2, 2, 2]),
                ],
                false,
            ))
            .unwrap();
        e.enable(id, &mut r).unwrap();
        e.disable(id, &mut r).unwrap();
        assert_eq!(r[0x200..0x208], before[..]);
    }

    #[test]
    fn conflicts_between_sources() {
        let mut e = engine();
        let mut r = ram();
        let a = e
            .register(set("a", vec![Patch::new(0x8000_0100, [1; 4])], false))
            .unwrap();
        let b = e
            .register(set("b", vec![Patch::new(0x8000_0102, [2; 4])], false))
            .unwrap();
        let c = e
            .register(set("b", vec![Patch::new(0x8000_0200, [3; 4])], false))
            .unwrap();
        e.enable(a, &mut r).unwrap();
        match e.enable(b, &mut r) {
            Err(PatchError::Conflict(v)) => {
                assert_eq!(v.len(), 1);
                assert_eq!((v[0].with, v[0].addr, v[0].len), (a, 0x102, 2));
                assert_eq!(v[0].with_source, "a");
            }
            other => panic!("{other:?}"),
        }
        assert!(!e.is_enabled(b));
        assert_eq!(r[0x104], 0x104u32 as u8, "refused set wrote nothing");
        e.enable(c, &mut r).unwrap();
        // disabled sets do not conflict; cached and uncached mirrors are the same bytes
        e.disable(a, &mut r).unwrap();
        e.enable(b, &mut r).unwrap();
        let d = e
            .register(set("d", vec![Patch::new(0xC000_0102, [4; 1])], false))
            .unwrap();
        assert!(matches!(e.enable(d, &mut r), Err(PatchError::Conflict(_))));
        // a source may overlap itself
        let b2 = e
            .register(set("b", vec![Patch::new(0x8000_0102, [9; 1])], false))
            .unwrap();
        e.enable(b2, &mut r).unwrap();
    }

    #[test]
    fn game_and_range_checks() {
        let mut e = PatchEngine::default();
        assert_eq!(
            e.set_game("NOPE00"),
            Err(PatchError::UnknownGame("NOPE00".into()))
        );
        assert_eq!(
            e.register(set("a", vec![], false)).unwrap_err(),
            PatchError::NoGame
        );
        e.set_game("GFEP01").unwrap();
        let mut other = set("a", vec![], false);
        other.game_id = "GFEJ01".into();
        assert!(matches!(
            e.register(other),
            Err(PatchError::WrongGame { .. })
        ));
        let end = 0x8000_0000 + RAM_SIZE as u32;
        assert!(matches!(
            e.register(set("a", vec![Patch::new(end - 2, [0; 4])], false)),
            Err(PatchError::OutOfRange { .. })
        ));
        assert!(e
            .register(set("a", vec![Patch::new(end - 4, [0; 4])], false))
            .is_ok());
    }

    #[test]
    fn frame_hook_runs_once_per_frame_and_rewrites_continuous_sets() {
        let mut e = engine();
        let mut r = ram();
        let hold = e
            .register(set("a", vec![Patch::word(0x300, 7)], true))
            .unwrap();
        let once = e
            .register(set("b", vec![Patch::word(0x400, 8)], false))
            .unwrap();
        e.enable(hold, &mut r).unwrap();
        e.enable(once, &mut r).unwrap();
        r[0x303] = 0;
        r[0x403] = 0;
        assert!(!e.frame(0, &mut r), "no retrace yet");
        assert!(e.frame(1, &mut r));
        assert!(!e.frame(1, &mut r), "same frame");
        assert_eq!(r[0x303], 7, "continuous set rewritten");
        assert_eq!(r[0x403], 0, "one-shot set left alone");
        assert!(e.frame(5, &mut r), "several retraces are one frame");
        assert_eq!(e.frames, 2);
        e.reapply(&mut r);
        assert_eq!(r[0x403], 8);
        e.disable(hold, &mut r).unwrap();
        r[0x303] = 0;
        e.frame(6, &mut r);
        assert_eq!(r[0x303], 0, "disabled set no longer written");
    }
}
