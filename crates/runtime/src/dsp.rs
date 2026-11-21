//! DSP interface (0xCC005000): mailboxes, control/status and the ARAM DMA registers.
//!
//! There is no DSP core: the mailbox protocol is answered directly (HLE). Audio mixing arrives
//! with M5.1; for boot the DSP only has to come out of reset and answer its ROM handshake.

const MAILBOX_TO_DSP_H: u32 = 0x00;
const MAILBOX_TO_DSP_L: u32 = 0x02;
const MAILBOX_FROM_DSP_H: u32 = 0x04;
const MAILBOX_FROM_DSP_L: u32 = 0x06;
const CONTROL: u32 = 0x0A;
const AR_MODE: u32 = 0x16;
const AR_DMA_MMADDR: u32 = 0x20;
const AR_DMA_ARADDR: u32 = 0x24;
const AR_DMA_CNT: u32 = 0x28;

/// 16 MiB of ARAM behind the DSP.
const ARAM_SIZE: usize = 0x0100_0000;

const CR_RESET: u16 = 1 << 0;
/// Interrupt status bits, cleared by writing 1: AI DMA (3), ARAM DMA (5), DSP (7).
const CR_W1C: u16 = (1 << 3) | (1 << 5) | (1 << 7);
const CR_HALT: u16 = 1 << 2;
const CR_INIT: u16 = 1 << 11;
const CR_ARINT: u16 = 1 << 5;

/// What the DSP ROM sends after reset to say it is ready for a boot task.
const ROM_READY_MAIL: u32 = 0x8071_FEED;
const MAIL_VALID: u32 = 0x8000_0000;

pub struct Dsp {
    regs: [u8; 0x100],
    control: u16,
    to_dsp: u32,
    from_dsp: u32,
    aram: Vec<u8>,
    trace: bool,
}

impl Dsp {
    pub fn new() -> Dsp {
        Dsp { regs: [0; 0x100], control: 0, to_dsp: 0, from_dsp: 0, aram: vec![0; ARAM_SIZE], trace: std::env::var("FEPOR_TRACE").is_ok_and(|v| v.split(',').any(|x| x == "dsp" || x == "all")) }
    }

    /// An unmasked interrupt flag is set (AI DMA, ARAM DMA or DSP).
    pub fn interrupt_pending(&self) -> bool {
        let c = self.control;
        (c & (1 << 3) != 0 && c & (1 << 4) != 0) || (c & (1 << 5) != 0 && c & (1 << 6) != 0) || (c & (1 << 7) != 0 && c & (1 << 8) != 0)
    }

    pub fn read(&mut self, off: u32, size: u32) -> u32 {
        match (off, size) {
            (CONTROL, 2) => self.control as u32,
            // bit 0: the ARAM controller is ready (the SDK spins on it after configuring the memory)
            (AR_MODE, 2) => self.raw_read(off, size) | 1,
            (MAILBOX_FROM_DSP_H, 2) => self.from_dsp >> 16,
            (MAILBOX_FROM_DSP_L, 2) => {
                let v = self.from_dsp & 0xFFFF;
                self.from_dsp &= !MAIL_VALID; // reading the low half consumes the mail
                v
            }
            (MAILBOX_TO_DSP_H, 2) => self.to_dsp >> 16,
            (MAILBOX_TO_DSP_L, 2) => self.to_dsp & 0xFFFF,
            _ => self.raw_read(off, size),
        }
    }

    /// `ram` is main memory, for the ARAM DMA.
    pub fn write(&mut self, off: u32, value: u32, size: u32, ram: &mut [u8]) {
        match (off, size) {
            (CONTROL, 2) => {
                let v = value as u16;
                let was_halted = self.control & CR_HALT != 0;
                let flags = self.control & CR_W1C & !(v & CR_W1C);
                self.control = (v & !CR_W1C & !CR_RESET & !CR_INIT) | flags;
                if was_halted && v & CR_HALT == 0 && v & CR_RESET == 0 {
                    // The boot task the SDK uploaded starts and reports in (contents are not inspected).
                    self.from_dsp = MAIL_VALID | 0x0001;
                }
                if v & CR_RESET != 0 {
                    // reset completes immediately and the ROM announces itself
                    self.from_dsp = ROM_READY_MAIL;
                }
                // DSPINIT is answered immediately as well: the DSP clears it when its init is done
            }
            (MAILBOX_TO_DSP_H, 2) => self.to_dsp = (self.to_dsp & 0xFFFF) | (value & 0x7FFF) << 16,
            (MAILBOX_TO_DSP_L, 2) => {
                // the DSP consumes mail immediately: the valid flag never stays set for the CPU to poll
                self.to_dsp = (self.to_dsp & 0x7FFF_0000) | (value & 0xFFFF);
                if self.trace {
                    eprintln!("dsp: mail from cpu {:#010x}", self.to_dsp | MAIL_VALID);
                }
            }
            (AR_DMA_CNT, 4) => {
                self.raw_write(off, value, size);
                self.aram_dma(ram);
            }
            _ => self.raw_write(off, value, size),
        }
    }

    /// Copy between main memory and ARAM; completes immediately and raises the ARAM interrupt flag.
    fn aram_dma(&mut self, ram: &mut [u8]) {
        let mm = (self.raw_read(AR_DMA_MMADDR, 4) & 0x03FF_FFFF) as usize;
        let ar = (self.raw_read(AR_DMA_ARADDR, 4) as usize) & (ARAM_SIZE - 1);
        let cnt = self.raw_read(AR_DMA_CNT, 4);
        let len = (cnt & 0x7FFF_FFFF) as usize;
        let n = len.min(ARAM_SIZE - ar).min(ram.len().saturating_sub(mm));
        if cnt & 0x8000_0000 == 0 {
            self.aram[ar..ar + n].copy_from_slice(&ram[mm..mm + n]);
        } else {
            ram[mm..mm + n].copy_from_slice(&self.aram[ar..ar + n]);
        }
        self.raw_write(AR_DMA_CNT, 0, 4);
        self.control |= CR_ARINT;
    }

    fn raw_read(&self, off: u32, size: u32) -> u32 {
        let (o, n) = (off as usize, size as usize);
        self.regs[o..o + n].iter().fold(0u32, |a, &b| a << 8 | b as u32)
    }

    fn raw_write(&mut self, off: u32, value: u32, size: u32) {
        let (o, n) = (off as usize, size as usize);
        for i in 0..n {
            self.regs[o + i] = (value >> (8 * (n - 1 - i))) as u8;
        }
    }
}

impl Default for Dsp {
    fn default() -> Self {
        Dsp::new()
    }
}
