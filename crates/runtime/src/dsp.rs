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
const AI_DMA_LEN: u32 = 0x36;
const AI_DMA_BYTES_LEFT: u32 = 0x3A;
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
const CR_DSPINT: u16 = 1 << 7;

/// Mail the boot task sends once the SDK's ucode is running (DSP_INIT).
const MAIL_DSP_INIT: u32 = 0xDCD1_0000;
/// Reply to the SDK's audio-frame mail (0xBABExxxx).
const MAIL_DSP_SYNC: u32 = 0xDCD1_0004;
const CR_AIDINT: u16 = 1 << 3;

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
    /// The previous CPU mail was the boot task's "start vector" command.
    expect_start: bool,
    /// Audio DMA: playing, buffer length in bytes and when the current buffer finishes.
    ai_dma_on: bool,
    ai_dma_len: u32,
    ai_next: Option<std::time::Instant>,
}

impl Dsp {
    pub fn new() -> Dsp {
        Dsp {
            regs: [0; 0x100],
            control: 0,
            to_dsp: 0,
            from_dsp: 0,
            aram: vec![0; ARAM_SIZE],
            trace: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "dsp" || x == "all")),
            expect_start: false,
            ai_dma_on: false,
            ai_dma_len: 0,
            ai_next: None,
        }
    }

    /// Advance the audio DMA clock: every finished buffer raises the AI DMA interrupt flag.
    pub fn tick(&mut self) {
        if !self.ai_dma_on || self.ai_dma_len == 0 {
            return;
        }
        // 16-bit stereo at 32 kHz: 4 bytes per sample
        let period =
            std::time::Duration::from_micros(self.ai_dma_len as u64 * 1_000_000 / (4 * 32_000));
        let now = std::time::Instant::now();
        let next = self.ai_next.get_or_insert(now + period);
        while now >= *next {
            *next += period;
            self.control |= CR_AIDINT;
        }
    }

    /// An unmasked interrupt flag is set (AI DMA, ARAM DMA or DSP).
    pub fn interrupt_pending(&self) -> bool {
        let c = self.control;
        (c & (1 << 3) != 0 && c & (1 << 4) != 0)
            || (c & (1 << 5) != 0 && c & (1 << 6) != 0)
            || (c & (1 << 7) != 0 && c & (1 << 8) != 0)
    }

    pub fn read(&mut self, off: u32, size: u32) -> u32 {
        match (off, size) {
            (CONTROL, 2) => self.control as u32,
            // bit 0: the ARAM controller is ready (the SDK spins on it after configuring the memory)
            (AR_MODE, 2) => self.raw_read(off, size) | 1,
            (AI_DMA_BYTES_LEFT, 2) => 0,
            (MAILBOX_FROM_DSP_H, 2) => self.from_dsp >> 16,
            (MAILBOX_FROM_DSP_L, 2) => {
                let v = self.from_dsp & 0xFFFF;
                if self.trace {
                    eprintln!("dsp: cpu reads mail {:#010x}", self.from_dsp);
                }
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
                if self.trace {
                    eprintln!("dsp: control <- {v:#06x} (was {:#06x})", self.control);
                }
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
                let mail = self.to_dsp | MAIL_VALID;
                if self.trace {
                    eprintln!("dsp: mail from cpu {mail:#010x}");
                }
                if self.expect_start {
                    // boot task complete: the uploaded ucode starts and announces itself with an interrupt
                    self.expect_start = false;
                    self.from_dsp = MAIL_DSP_INIT;
                    self.control |= CR_DSPINT;
                } else if mail == 0x80F3_D001 {
                    self.expect_start = true;
                } else if mail >> 16 == 0xBABE {
                    // audio frame request: the AX ucode answers with a sync mail and an interrupt
                    self.from_dsp = MAIL_DSP_SYNC;
                    self.control |= CR_DSPINT;
                }
            }
            (AR_DMA_CNT, 4) => {
                self.raw_write(off, value, size);
                self.aram_dma(ram);
            }
            (0x2A, 2) => {
                // the SDK writes the count as two halves; the low one starts the transfer
                self.raw_write(off, value, size);
                if self.raw_read(AR_DMA_CNT, 4) & 0x7FFF_FFFF != 0 {
                    self.aram_dma(ram);
                }
            }
            (AI_DMA_LEN, 2) => {
                self.raw_write(off, value, size);
                self.ai_dma_on = value & 0x8000 != 0;
                self.ai_dma_len = (value & 0x7FFF) * 32;
                if !self.ai_dma_on {
                    self.ai_next = None;
                }
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
        if self.trace {
            eprintln!(
                "dsp: aram dma {} main {mm:#x} aram {ar:#x} len {len:#x}",
                if cnt >> 31 != 0 {
                    "aram->main"
                } else {
                    "main->aram"
                }
            );
        }
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
        self.regs[o..o + n]
            .iter()
            .fold(0u32, |a, &b| a << 8 | b as u32)
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
