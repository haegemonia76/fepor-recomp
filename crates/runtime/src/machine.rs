use std::cell::UnsafeCell;
use std::time::Instant;

use crate::hw::Hw;
use crate::trace::Trace;

/// 24 MiB main RAM (MEM1).
pub const RAM_SIZE: usize = 0x0180_0000;
/// Extra bytes so a multi-byte access at the very end cannot leave the allocation.
const RAM_SLACK: usize = 64;

pub struct Machine {
    pub ram: Box<[u8]>,
    pub hw: Hw,
    pub trace: Trace,
    pub started: Instant,
    /// Function entries seen (interrupt polling cadence).
    pub enter_count: u32,
}

impl Machine {
    pub fn new() -> Machine {
        Machine { ram: vec![0u8; RAM_SIZE + RAM_SLACK].into_boxed_slice(), hw: Hw::new(), trace: Trace::from_env(), started: Instant::now(), enter_count: 0 }
    }

    pub fn ram_ptr(&mut self) -> *mut u8 {
        self.ram.as_mut_ptr()
    }

    fn phys(ea: u32) -> usize {
        (ea & 0x3FFF_FFFF) as usize
    }

    pub fn read_u32(&self, ea: u32) -> u32 {
        let p = Self::phys(ea);
        u32::from_be_bytes(self.ram[p..p + 4].try_into().unwrap())
    }

    pub fn write_u32(&mut self, ea: u32, v: u32) {
        let p = Self::phys(ea);
        self.ram[p..p + 4].copy_from_slice(&v.to_be_bytes());
    }

    pub fn write_bytes(&mut self, ea: u32, data: &[u8]) {
        let p = Self::phys(ea);
        self.ram[p..p + data.len()].copy_from_slice(data);
    }

    /// Time base ticks (the Gekko time base runs at bus clock / 4 = 40.5 MHz).
    pub fn timebase(&self) -> u64 {
        (self.started.elapsed().as_nanos() * 405 / 10_000) as u64
    }
}

impl Default for Machine {
    fn default() -> Self {
        Machine::new()
    }
}

struct Global(UnsafeCell<Option<Machine>>);
// SAFETY: the guest runs on one host thread; the machine is only touched from it.
unsafe impl Sync for Global {}
static MACHINE: Global = Global(UnsafeCell::new(None));

/// The process-wide machine, created on first use. Only call from the guest thread.
#[allow(clippy::mut_from_ref)]
pub fn machine() -> &'static mut Machine {
    // SAFETY: single guest thread, see `Global`.
    unsafe { (*MACHINE.0.get()).get_or_insert_with(Machine::new) }
}
