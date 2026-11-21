//! Development tracing, selected with `FEPOR_TRACE=hw,calls,irq,vi` (comma separated).

pub const RING: usize = 4096;

#[derive(Default)]
pub struct Trace {
    pub hw: bool,
    pub calls: bool,
    pub irq: bool,
    ring: Vec<u32>,
    next: usize,
}

impl Trace {
    pub fn from_env() -> Trace {
        let v = std::env::var("FEPOR_TRACE").unwrap_or_default();
        let has = |k: &str| v.split(',').any(|x| x == k || x == "all");
        Trace { hw: has("hw"), calls: has("calls"), irq: has("irq"), ring: vec![0; RING], next: 0 }
    }

    pub fn enter(&mut self, addr: u32) {
        self.ring[self.next % RING] = addr;
        self.next += 1;
    }

    /// The most recent function entries, oldest first.
    pub fn recent(&self, n: usize) -> Vec<u32> {
        let n = n.min(self.next).min(RING);
        (self.next - n..self.next).map(|i| self.ring[i % RING]).collect()
    }
}
