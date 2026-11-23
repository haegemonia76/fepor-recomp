//! Development tracing, selected with `FEPOR_TRACE=hw,calls,irq,vi` (comma separated).

pub const RING: usize = 4096;

#[derive(Default)]
pub struct Trace {
    pub hw: bool,
    pub calls: bool,
    pub irq: bool,
    /// `FEPOR_WATCH=8021952c,...`: print the arguments every time one of these functions is entered.
    pub watch: Vec<u32>,
    /// `FEPOR_WATCH_MEM=<hex address>` (with `FEPOR_TRACE=calls`): print every change of this word and the calls
    /// that ran just before the change was seen.
    pub watch_mem: Option<u32>,
    pub watch_mem_last: u32,
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
            watch_mem: std::env::var("FEPOR_WATCH_MEM")
                .ok()
                .and_then(|a| u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok()),
            watch_mem_last: 0,
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
        if self.next.is_multiple_of(1 << 24) {
            self.print_hot();
        }
    }

    /// The most frequent callees among the last `RING` entries: where the guest spends its time.
    fn print_hot(&self) {
        let mut h: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for a in &self.ring {
            *h.entry(*a).or_default() += 1;
        }
        let mut v: Vec<_> = h.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let top: Vec<String> = v
            .iter()
            .take(10)
            .map(|(a, n)| format!("{a:08x}x{n}"))
            .collect();
        eprintln!("hot: {}", top.join(" "));
    }

    /// The most recent function entries, oldest first.
    pub fn recent(&self, n: usize) -> Vec<u32> {
        let n = n.min(self.next).min(RING);
        (self.next - n..self.next)
            .map(|i| self.ring[i % RING])
            .collect()
    }
}
