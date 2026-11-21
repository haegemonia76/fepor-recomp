//! Disc Interface (0xCC006000): the DVD drive, answered by reading the player's disc image directly.
//!
//! Commands complete immediately. Only what the SDK uses is modelled: inquiry, read, seek, and
//! the status/interrupt registers. Interrupt delivery to the CPU is driven by the PI (later).

use fepor_core::Disc;

const DISR: u32 = 0x00;
const DICVR: u32 = 0x04;
const DICMDBUF0: u32 = 0x08;
const DICMDBUF1: u32 = 0x0C;
const DICMDBUF2: u32 = 0x10;
const DIMAR: u32 = 0x14;
const DILENGTH: u32 = 0x18;
const DICR: u32 = 0x1C;
const DIIMMBUF: u32 = 0x20;
const DICFG: u32 = 0x24;

const SR_BRKINT: u32 = 1 << 6;
const SR_TCINT: u32 = 1 << 4;
const SR_DEINT: u32 = 1 << 2;
/// Interrupt flags clear when written with 1.
const SR_W1C: u32 = SR_BRKINT | SR_TCINT | SR_DEINT;
const CVR_INT: u32 = 1 << 2;

pub struct Di {
    disc: Option<Box<dyn Disc>>,
    regs: [u32; 10],
    status: u32,
    cover: u32,
    trace: bool,
}

impl Di {
    pub fn new() -> Di {
        Di { disc: None, regs: [0; 10], status: 0, cover: 0, trace: std::env::var("FEPOR_TRACE").is_ok_and(|v| v.split(',').any(|x| x == "di" || x == "all")) }
    }

    pub fn attach(&mut self, disc: Box<dyn Disc>) {
        self.disc = Some(disc);
    }

    /// Interrupt cause bits (any unmasked flag set).
    pub fn interrupt_pending(&self) -> bool {
        let mask = (self.status >> 1 & 1) << 2 | (self.status >> 3 & 1) << 4 | (self.status >> 5 & 1) << 6;
        self.status & SR_W1C & mask != 0 || (self.cover & CVR_INT != 0 && self.cover & 2 != 0)
    }

    pub fn read(&mut self, off: u32, _size: u32) -> u32 {
        match off {
            DISR => self.status,
            DICVR => self.cover,
            DICFG => 1,
            o if o <= DIIMMBUF => self.regs[(o / 4) as usize],
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, value: u32, ram: &mut [u8]) {
        match off {
            DISR => {
                // masks are plain storage, flags clear on write of 1
                self.status = (value & !SR_W1C) | (self.status & SR_W1C & !(value & SR_W1C));
            }
            DICVR => self.cover = (value & 2) | (self.cover & CVR_INT & !(value & CVR_INT)),
            DICR => {
                self.regs[(DICR / 4) as usize] = value;
                if value & 1 != 0 {
                    self.execute(value, ram);
                    self.regs[(DICR / 4) as usize] &= !1;
                    self.status |= SR_TCINT;
                }
            }
            o if o <= DIIMMBUF => self.regs[(o / 4) as usize] = value,
            _ => {}
        }
    }

    fn execute(&mut self, cr: u32, ram: &mut [u8]) {
        let cmd = self.regs[(DICMDBUF0 / 4) as usize];
        let dma = cr & 2 != 0;
        let mar = (self.regs[(DIMAR / 4) as usize] & 0x03FF_FFE0) as usize;
        let len = self.regs[(DILENGTH / 4) as usize] as usize;
        if self.trace {
            eprintln!("di: cmd {cmd:#010x} arg1 {:#x} arg2 {:#x} mar {mar:#x} len {len:#x} dma {dma}", self.regs[(DICMDBUF1 / 4) as usize], self.regs[(DICMDBUF2 / 4) as usize]);
        }
        match cmd >> 24 {
            0x12 => {
                // inquiry: drive revision information
                if dma && mar + len <= ram.len() {
                    ram[mar..mar + len].fill(0);
                }
            }
            0xA8 => {
                // read: offset in words in CMDBUF1, length in CMDBUF2
                let offset = self.regs[(DICMDBUF1 / 4) as usize] as u64 * 4;
                let n = self.regs[(DICMDBUF2 / 4) as usize] as usize;
                if let Some(disc) = self.disc.as_mut() {
                    if dma && mar + len <= ram.len() && n <= len {
                        if let Err(e) = disc.read_at(offset, &mut ram[mar..mar + n]) {
                            crate::abi::die_str(&format!("disc read at {offset:#x}: {e}"));
                        }
                    }
                }
            }
            _ => {} // seek, stop motor, audio streaming: nothing to do
        }
        if dma {
            // the driver measures how much was transferred from what remains
            self.regs[(DIMAR / 4) as usize] = self.regs[(DIMAR / 4) as usize].wrapping_add(len as u32);
            self.regs[(DILENGTH / 4) as usize] = 0;
        }
        self.regs[(DIIMMBUF / 4) as usize] = 0;
        self.regs[(DICMDBUF1 / 4) as usize] = self.regs[(DICMDBUF1 / 4) as usize];
        let _ = DICMDBUF2;
    }
}

impl Default for Di {
    fn default() -> Self {
        Di::new()
    }
}
