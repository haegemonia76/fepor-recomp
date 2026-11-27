//! Save states: what the front ends (hotkeys, overlay) ask for and where the files live (ADR 0035).
//! The runtime executes the commands at a safe point of the guest (`fepor_runtime::state`).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;

/// Most memory the rewind buffer may use (compressed differences plus the newest state).
pub const REWIND_MEMORY_MIB: u32 = 512;

/// Number of slots.
pub const SLOTS: u8 = 8;

const MAGIC: &[u8; 8] = b"FEPORST\x01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Save into slot (0-based).
    Save(u8),
    Load(u8),
}

static QUEUE: Mutex<VecDeque<Command>> = Mutex::new(VecDeque::new());
static PENDING: AtomicBool = AtomicBool::new(false);
static SLOT: AtomicU8 = AtomicU8::new(0);
static REWIND_HOLD: AtomicBool = AtomicBool::new(false);
static REWIND_SECONDS: AtomicU32 = AtomicU32::new(0);
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static MESSAGE: Mutex<Option<(Instant, String)>> = Mutex::new(None);

/// Ask the runtime for a save or a load; it happens at the guest's next safe point.
pub fn post(c: Command) {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push_back(c);
    PENDING.store(true, Ordering::Release);
}

/// Is a command waiting? (One atomic load: the guest asks at every poll point.)
pub fn pending() -> bool {
    PENDING.load(Ordering::Acquire)
}

/// The next command, left in the queue.
pub fn peek() -> Option<Command> {
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .front()
        .copied()
}

pub fn take() -> Option<Command> {
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    let c = q.pop_front();
    if q.is_empty() {
        PENDING.store(false, Ordering::Release);
    }
    c
}

/// The slot the quick save and quick load keys use (0-based).
pub fn slot() -> u8 {
    SLOT.load(Ordering::Relaxed)
}

pub fn set_slot(s: u8) {
    SLOT.store(s % SLOTS, Ordering::Relaxed);
}

/// The rewind key is held.
pub fn rewind_held() -> bool {
    REWIND_HOLD.load(Ordering::Relaxed)
}

pub fn set_rewind_held(v: bool) {
    REWIND_HOLD.store(v, Ordering::Relaxed);
}

/// Seconds of play the rewind buffer holds (published by the runtime for the overlay).
pub fn rewind_seconds() -> u32 {
    REWIND_SECONDS.load(Ordering::Relaxed)
}

pub fn set_rewind_seconds(s: u32) {
    REWIND_SECONDS.store(s, Ordering::Relaxed);
}

/// Where this game's states are kept: `<data>/states/<game id>-v<disc version>`.
pub fn set_dir(dir: PathBuf) {
    *DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
}

pub fn slot_path(slot: u8) -> Option<PathBuf> {
    DIR.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|d| d.join(format!("slot{}.fst", slot + 1)))
}

/// A line for the player: shown on screen for a few seconds.
pub fn set_message(text: impl Into<String>) {
    *MESSAGE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), text.into()));
}

pub fn message() -> Option<String> {
    let m = MESSAGE.lock().unwrap_or_else(|e| e.into_inner());
    m.as_ref()
        .filter(|(t, _)| t.elapsed().as_secs() < 4)
        .map(|(_, s)| s.clone())
}

pub struct Info {
    pub modified: SystemTime,
    pub bytes: u64,
}

/// What each slot holds.
pub fn list() -> Vec<Option<Info>> {
    (0..SLOTS)
        .map(|s| {
            let md = std::fs::metadata(slot_path(s)?).ok()?;
            Some(Info {
                modified: md.modified().ok()?,
                bytes: md.len(),
            })
        })
        .collect()
}

/// Write a state file: a header naming the game (`id`), then the compressed state, through a temporary
/// file so that a failed write leaves the old state alone.
pub fn write_file(path: &std::path::Path, id: &[u8; 8], blob: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        f.write_all(MAGIC)?;
        f.write_all(id)?;
        let mut z = DeflateEncoder::new(f, Compression::fast());
        z.write_all(blob)?;
        z.finish()?.flush()?;
    }
    std::fs::rename(tmp, path)
}

/// Read a state file written for the game `id`.
pub fn read_file(path: &std::path::Path, id: &[u8; 8]) -> Result<Vec<u8>, String> {
    let mut f = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| e.to_string())?);
    let mut head = [0u8; 16];
    f.read_exact(&mut head).map_err(|e| e.to_string())?;
    if &head[..8] != MAGIC {
        return Err("not a save state of this program".into());
    }
    if &head[8..] != id {
        return Err("this state belongs to another game or disc version".into());
    }
    let mut out = Vec::new();
    DeflateDecoder::new(f)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip_checks_the_game() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.fst");
        let blob: Vec<u8> = (0..100_000u32).map(|i| (i % 7) as u8).collect();
        write_file(&p, b"GFEP0100", &blob).unwrap();
        assert_eq!(read_file(&p, b"GFEP0100").unwrap(), blob);
        assert!(read_file(&p, b"GFEE0100").is_err());
    }
}
