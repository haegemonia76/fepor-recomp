//! The memory watcher's shared pieces (ADR 0048): the RAM scan, the export of a found address as a cheat entry
//! or a Gecko code, and the [`Link`] through which the overlay (render thread) and the guest thread talk.
//!
//! Nothing here touches the guest's memory by itself: the scan works on a RAM slice it is given, and the
//! [`Link`] only carries requests one way and published copies the other.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Size of a searched value: big-endian like the guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    U8,
    U16,
    U32,
}

impl Width {
    pub const ALL: [Width; 3] = [Width::U8, Width::U16, Width::U32];

    pub fn bytes(self) -> usize {
        match self {
            Width::U8 => 1,
            Width::U16 => 2,
            Width::U32 => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Width::U8 => "8 bit",
            Width::U16 => "16 bit",
            Width::U32 => "32 bit",
        }
    }

    pub fn parse(s: &str) -> Option<Width> {
        match s {
            "u8" | "8" => Some(Width::U8),
            "u16" | "16" => Some(Width::U16),
            "u32" | "32" => Some(Width::U32),
            _ => None,
        }
    }

    /// Largest value of this width.
    pub fn max(self) -> u32 {
        match self {
            Width::U8 => 0xFF,
            Width::U16 => 0xFFFF,
            Width::U32 => u32::MAX,
        }
    }

    /// The value at the start of `bytes` (at least `self.bytes()` long).
    pub fn read(self, bytes: &[u8]) -> u32 {
        bytes[..self.bytes()]
            .iter()
            .fold(0, |v, b| (v << 8) | *b as u32)
    }

    /// `value` as big-endian bytes of this width (high bits beyond the width dropped).
    pub fn bytes_of(self, value: u32) -> Vec<u8> {
        value.to_be_bytes()[4 - self.bytes()..].to_vec()
    }
}

/// How the next scan step keeps candidates, comparing the value now with the one at the previous step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Equal(u32),
    Changed,
    Unchanged,
    Increased,
    Decreased,
}

impl Filter {
    fn keep(self, before: u32, now: u32) -> bool {
        match self {
            Filter::Equal(x) => now == x,
            Filter::Changed => now != before,
            Filter::Unchanged => now == before,
            Filter::Increased => now > before,
            Filter::Decreased => now < before,
        }
    }

    /// `equal 5`, `changed`, `unchanged`, `increased`, `decreased` (also `inc`, `dec`); the value is decimal or
    /// `0x` hex.
    pub fn parse(s: &str) -> Option<Filter> {
        let mut it = s.split_whitespace();
        let f = match it.next()? {
            "equal" | "eq" => Filter::Equal(parse_number(it.next()?)?),
            "changed" => Filter::Changed,
            "unchanged" => Filter::Unchanged,
            "increased" | "inc" => Filter::Increased,
            "decreased" | "dec" => Filter::Decreased,
            _ => return None,
        };
        it.next().is_none().then_some(f)
    }
}

/// Decimal, or hexadecimal with a `0x` prefix.
pub fn parse_number(s: &str) -> Option<u32> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

/// A candidate address of a scan, with its value at the last step and now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Cached address (`0x80…`).
    pub addr: u32,
    pub at_scan: u32,
    pub now: u32,
}

/// A search for values in RAM that narrows with each step. The first step either looks for a value or just
/// takes a snapshot ("unknown initial value"); later steps keep the addresses whose value behaved as asked
/// since the step before. Addresses are multiples of the width (the guest aligns its data).
pub struct Scanner {
    width: Width,
    /// RAM at the last step.
    snapshot: Vec<u8>,
    /// Candidate offsets into RAM; `None` while every aligned address still is one.
    cands: Option<Vec<u32>>,
}

impl Scanner {
    /// Start a scan: `equal` keeps the addresses holding that value, `None` keeps them all.
    pub fn start(ram: &[u8], width: Width, equal: Option<u32>) -> Scanner {
        let mut s = Scanner {
            width,
            snapshot: ram.to_vec(),
            cands: None,
        };
        if let Some(x) = equal {
            s.cands = Some(s.filter(ram, Filter::Equal(x)));
        }
        s
    }

    fn filter(&self, ram: &[u8], f: Filter) -> Vec<u32> {
        let w = self.width;
        let keep = |off: usize| f.keep(w.read(&self.snapshot[off..]), w.read(&ram[off..]));
        match &self.cands {
            Some(c) => c.iter().copied().filter(|&o| keep(o as usize)).collect(),
            None => (0..=ram.len() - w.bytes())
                .step_by(w.bytes())
                .filter(|&o| keep(o))
                .map(|o| o as u32)
                .collect(),
        }
    }

    /// Keep the candidates that satisfy `f` against the RAM now; the RAM now becomes the reference of the next step.
    pub fn narrow(&mut self, ram: &[u8], f: Filter) {
        self.cands = Some(self.filter(ram, f));
        self.snapshot.copy_from_slice(ram);
    }

    pub fn width(&self) -> Width {
        self.width
    }

    /// Candidates left (every aligned address before the first narrowing of an unknown-value scan).
    pub fn count(&self) -> usize {
        self.cands
            .as_ref()
            .map_or(self.snapshot.len() / self.width.bytes(), Vec::len)
    }

    /// The first `limit` candidates in address order; none while every address still is one.
    pub fn hits(&self, ram: &[u8], limit: usize) -> Vec<Hit> {
        let w = self.width;
        self.cands
            .iter()
            .flatten()
            .take(limit)
            .map(|&o| {
                let o = o as usize;
                Hit {
                    addr: 0x8000_0000 | o as u32,
                    at_scan: w.read(&self.snapshot[o..]),
                    now: w.read(&ram[o..]),
                }
            })
            .collect()
    }
}

/// The address as a cheat entry for the native cheats (TOML, ticket #33): `[[cheat]]` with one `[[cheat.write]]`
/// of `address`, `size` (1, 2 or 4 bytes) and `value`, held every frame when `hold`. `game` is the game ID of the
/// addresses (GFEP01, GFEJ01), when known.
pub fn cheat_toml(
    name: &str,
    game: Option<&str>,
    addr: u32,
    width: Width,
    value: u32,
    hold: bool,
) -> String {
    let mut s = format!("[[cheat]]\nname = {}\n", toml_str(name));
    if let Some(g) = game {
        s += &format!("game = {}\n", toml_str(g));
    }
    if hold {
        s += "continuous = true\n";
    }
    s += &format!(
        "\n[[cheat.write]]\naddress = {addr:#010x}\nsize = {}\nvalue = {value:#0w$x}\n",
        width.bytes(),
        w = 2 + 2 * width.bytes()
    );
    s
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The Gecko code line (RAM write codes `00`, `02`, `04`; `01`, `03`, `05` for the `0x81…` half of RAM).
pub fn gecko_line(addr: u32, width: Width, value: u32) -> String {
    let kind = match width {
        Width::U8 => 0x0000_0000u32,
        Width::U16 => 0x0200_0000,
        Width::U32 => 0x0400_0000,
    };
    format!(
        "{:08X} {:08X}",
        kind | (addr & 0x01FF_FFFF),
        value & width.max()
    )
}

/// What the overlay asks of the guest thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Begin a scan (replacing the one running): look for `equal`, or snapshot when `None`.
    NewScan {
        width: Width,
        equal: Option<u32>,
    },
    Narrow(Filter),
    ClearScan,
    /// Write `value` at `addr`; `hold` writes it again every frame (through the patch engine) until released.
    Poke {
        addr: u32,
        width: Width,
        value: u32,
        hold: bool,
    },
    /// Stop holding the value at `addr`.
    Release(u32),
    /// The bytes to publish for the hex view.
    View {
        addr: u32,
        len: u32,
    },
    /// The values to publish live.
    Watch(Vec<(u32, Width)>),
}

/// A held value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub addr: u32,
    pub width: Width,
    pub value: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ScanInfo {
    pub width: Option<Width>,
    pub count: usize,
    pub hits: Vec<Hit>,
}

/// What the guest thread publishes after each frame it served.
#[derive(Clone, Debug, Default)]
pub struct Published {
    /// Frames served so far.
    pub frame: u64,
    /// The running game's ID, once known.
    pub game: Option<String>,
    pub scan: Option<ScanInfo>,
    pub view_addr: u32,
    pub view: Vec<u8>,
    /// The watched addresses with their value now (`None`: outside RAM).
    pub watches: Vec<(u32, Width, Option<u32>)>,
    pub held: Vec<Held>,
    /// The last result or error of a request.
    pub message: String,
}

/// The channel between the overlay and the guest thread: requests go one way, copies come back the other.
/// Neither side ever touches the other's memory.
#[derive(Default)]
pub struct Link {
    requests: Mutex<Vec<Request>>,
    published: Mutex<Published>,
    /// The memory page is shown: the guest thread publishes. Otherwise it only serves requests.
    active: AtomicBool,
}

impl Link {
    pub fn send(&self, r: Request) {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(r);
    }

    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn publish(&self, p: Published) {
        *self.published.lock().unwrap_or_else(|e| e.into_inner()) = p;
    }

    pub fn published(&self) -> Published {
        self.published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ram(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }

    #[test]
    fn equal_start_finds_the_value_at_aligned_addresses() {
        let r = ram(&[7, 9, 7, 0]);
        let s = Scanner::start(&r, Width::U32, Some(7));
        assert_eq!(s.count(), 2);
        let h = s.hits(&r, 10);
        assert_eq!(
            h.iter().map(|h| h.addr).collect::<Vec<_>>(),
            [0x8000_0000, 0x8000_0008]
        );
        // 16-bit halves: only the aligned ones are looked at
        let s = Scanner::start(&r, Width::U16, Some(7));
        assert_eq!(s.count(), 2);
    }

    #[test]
    fn unknown_start_then_changed_and_unchanged() {
        let mut r = ram(&[1, 2, 3, 4]);
        let mut s = Scanner::start(&r, Width::U32, None);
        assert_eq!(s.count(), 4);
        assert!(
            s.hits(&r, 10).is_empty(),
            "nothing to list before the first narrowing"
        );
        r[7] = 9; // word 1: 2 -> 9
        r[15] = 0; // word 3: 4 -> 0
        s.narrow(&r, Filter::Changed);
        assert_eq!(
            s.hits(&r, 10)
                .iter()
                .map(|h| (h.addr, h.at_scan, h.now))
                .collect::<Vec<_>>(),
            [(0x8000_0004, 9, 9), (0x8000_000C, 0, 0)]
        );
        r[15] = 5; // only word 3 moves again
        s.narrow(&r, Filter::Unchanged);
        assert_eq!(s.count(), 1);
        assert_eq!(s.hits(&r, 10)[0].addr, 0x8000_0004);
        s.narrow(&r, Filter::Equal(1));
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn increased_and_decreased_compare_unsigned_with_the_last_step() {
        let mut r = ram(&[10, 10, 0xFFFF_FFF0]);
        let mut s = Scanner::start(&r, Width::U32, None);
        r.copy_from_slice(&ram(&[11, 9, 5]));
        s.narrow(&r, Filter::Increased);
        assert_eq!(
            s.hits(&r, 10).iter().map(|h| h.addr).collect::<Vec<_>>(),
            [0x8000_0000]
        );
        let mut s = Scanner::start(&ram(&[10, 10, 0xFFFF_FFF0]), Width::U32, None);
        s.narrow(&r, Filter::Decreased);
        assert_eq!(
            s.hits(&r, 10).iter().map(|h| h.addr).collect::<Vec<_>>(),
            [0x8000_0004, 0x8000_0008]
        );
    }

    #[test]
    fn steps_compare_with_the_step_before_not_the_first() {
        let mut r = ram(&[1]);
        let mut s = Scanner::start(&r, Width::U32, None);
        r.copy_from_slice(&ram(&[2]));
        s.narrow(&r, Filter::Increased);
        r.copy_from_slice(&ram(&[3]));
        s.narrow(&r, Filter::Increased);
        assert_eq!(s.count(), 1);
        s.narrow(&r, Filter::Increased);
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn byte_scan_reads_single_bytes_and_hits_are_limited() {
        let r = vec![5u8; 64];
        let s = Scanner::start(&r, Width::U8, Some(5));
        assert_eq!(s.count(), 64);
        assert_eq!(s.hits(&r, 3).len(), 3);
    }

    #[test]
    fn width_round_trips_big_endian() {
        assert_eq!(Width::U16.read(&[0x12, 0x34, 0x56]), 0x1234);
        assert_eq!(Width::U32.bytes_of(0xAABB_CCDD), [0xAA, 0xBB, 0xCC, 0xDD]);
        assert_eq!(Width::U8.bytes_of(0x1FF), [0xFF]);
    }

    #[test]
    fn filters_and_numbers_parse() {
        assert_eq!(Filter::parse("equal 0x10"), Some(Filter::Equal(16)));
        assert_eq!(Filter::parse("dec"), Some(Filter::Decreased));
        assert_eq!(Filter::parse("equal"), None);
        assert_eq!(Filter::parse("changed 3"), None);
        assert_eq!(parse_number(" 42 "), Some(42));
    }

    #[test]
    fn exports() {
        assert_eq!(gecko_line(0x8012_3456, Width::U32, 99), "04123456 00000063");
        assert_eq!(
            gecko_line(0x8112_3456, Width::U16, 0x1_0005),
            "03123456 00000005"
        );
        assert_eq!(
            gecko_line(0x8000_0010, Width::U8, 0x1FF),
            "00000010 000000FF"
        );
        let t = cheat_toml(
            "HP \"max\"",
            Some("GFEP01"),
            0x8012_3456,
            Width::U16,
            99,
            true,
        );
        assert!(t.contains("name = \"HP \\\"max\\\"\""));
        assert!(t.contains("continuous = true"));
        assert!(t.contains("address = 0x80123456"));
        assert!(t.contains("size = 2"));
        assert!(t.contains("value = 0x0063"));
        // the snippet is valid TOML
        let v: toml::Table = toml::from_str(&t).unwrap();
        assert_eq!(
            v["cheat"][0]["write"][0]["address"].as_integer(),
            Some(0x8012_3456)
        );
    }
}
