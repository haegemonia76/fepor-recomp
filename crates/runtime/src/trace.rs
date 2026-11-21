//! Development tracing, selected with `FEPOR_TRACE=hw,calls,irq,vi` (comma separated).

pub const RING: usize = 4096;

#[derive(Default)]
pub struct Trace {
    pub hw: bool,
    pub calls: bool,
    pub irq: bool,
    /// `FEPOR_WATCH=8021952c,...`: print the arguments every time one of these functions is entered.
    pub watch: Vec<u32>,
    ring: Vec<u32>,
    next: usize,
    /// Functions in order of first execution (`FEPOR_TRACE=calls`): the tail shows the newest code paths.
    seen: std::collections::HashSet<u32>,
    pub first_seen: Vec<u32>,
    /// `FEPOR_MARK_DI=<hex word offset>`: remember where in `first_seen` the first DVD read at or past that
    /// offset happened, so a crash/watchdog report lists the code that ran after it.
    mark_di: Option<u32>,
    pub mark: Option<usize>,
    /// The first calls after the mark, in order.
    pub after_mark: Vec<u32>,
}

impl Trace {
    pub fn from_env() -> Trace {
        let v = std::env::var("FEPOR_TRACE").unwrap_or_default();
        let has = |k: &str| v.split(',').any(|x| x == k || x == "all");
        Trace {
            hw: has("hw"),
            calls: has("calls"),
            irq: has("irq"),
            watch: std::env::var("FEPOR_WATCH")
                .unwrap_or_default()
                .split(',')
                .filter_map(|a| u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok())
                .collect(),
            ring: vec![0; RING],
            next: 0,
            seen: Default::default(),
            first_seen: Vec::new(),
            mark_di: std::env::var("FEPOR_MARK_DI")
                .ok()
                .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()),
            mark: None,
            after_mark: Vec::new(),
        }
    }

    /// Called for every DVD read (word offset on the disc).
    pub fn di_read(&mut self, word_offset: u32) {
        if self.mark.is_none() && self.mark_di.is_some_and(|m| word_offset >= m) {
            self.mark = Some(self.first_seen.len());
        }
    }

    pub fn enter(&mut self, addr: u32) {
        if self.mark.is_some() && self.after_mark.len() < 6000 {
            self.after_mark.push(addr);
        }
        if self.seen.insert(addr) {
            self.first_seen.push(addr);
        }
        self.ring[self.next % RING] = addr;
        self.next += 1;
    }

    /// The most recent function entries, oldest first.
    pub fn recent(&self, n: usize) -> Vec<u32> {
        let n = n.min(self.next).min(RING);
        (self.next - n..self.next)
            .map(|i| self.ring[i % RING])
            .collect()
    }
}
