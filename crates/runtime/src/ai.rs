//! Audio Interface (0xCC006C00): control register and the sample counter the SDK uses to measure
//! the sample rate. No audio is produced yet (M5.1).

use std::time::Instant;

const AICR: u32 = 0x00;
const AISCNT: u32 = 0x08;

const CR_PSTAT: u32 = 1 << 0;
const CR_AIINT: u32 = 1 << 3;
const CR_SCRESET: u32 = 1 << 5;
const CR_AIDFR: u32 = 1 << 6;

pub struct Ai {
    regs: [u32; 8],
    control: u32,
    /// Samples counted before the current play period.
    base: u64,
    playing_since: Option<Instant>,
}

impl Ai {
    pub fn new() -> Ai {
        Ai {
            regs: [0; 8],
            control: 0,
            base: 0,
            playing_since: None,
        }
    }

    /// DSP sample rate: AICR.AIDFR (bit 6) set selects 32 kHz, clear 48 kHz.
    pub fn rate(&self) -> u64 {
        if self.control & CR_AIDFR != 0 {
            32_000
        } else {
            48_000
        }
    }

    fn counter(&self) -> u32 {
        let live = self.playing_since.map_or(0, |t| {
            (t.elapsed().as_micros() as u64 * self.rate()) / 1_000_000
        });
        (self.base + live) as u32
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            AICR => self.control,
            AISCNT => self.counter(),
            o => self.regs[(o / 4) as usize & 7],
        }
    }

    pub fn write(&mut self, off: u32, value: u32) {
        match off {
            AICR => {
                if value & CR_SCRESET != 0 {
                    self.base = 0;
                    if self.playing_since.is_some() {
                        self.playing_since = Some(Instant::now());
                    }
                }
                let playing = value & CR_PSTAT != 0;
                if playing && self.playing_since.is_none() {
                    self.playing_since = Some(Instant::now());
                } else if !playing {
                    self.base = self.counter() as u64;
                    self.playing_since = None;
                }
                // AIINT clears on write of 1; SCRESET is a strobe
                self.control =
                    (value & !CR_AIINT & !CR_SCRESET) | (self.control & CR_AIINT & !value);
            }
            o => self.regs[(o / 4) as usize & 7] = value,
        }
    }
}

impl Default for Ai {
    fn default() -> Self {
        Ai::new()
    }
}

impl Ai {
    pub fn save(&self, w: &mut fepor_core::snap::W) {
        w.u32s(&self.regs);
        w.u32(self.control);
        w.u64(self.base);
        w.bool(self.playing_since.is_some());
        w.u64(self.playing_since.map_or(0, |t| t.elapsed().as_micros() as u64));
    }

    pub fn load(&mut self, r: &mut fepor_core::snap::R) {
        r.u32s_into(&mut self.regs);
        self.control = r.u32();
        self.base = r.u64();
        let playing = r.bool();
        let since = r.u64();
        self.playing_since = playing.then(|| crate::state::ago(since));
    }
}
