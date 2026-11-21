//! Hardware registers at 0xCC000000 (physical 0x0C000000..0x0C009000).
//!
//! Registers start as plain storage (what the SDK writes it can read back) and get dedicated
//! behaviour as boot needs it (EXI so far). Every access can be traced with `FEPOR_TRACE=hw`.

use crate::di::Di;
use crate::dsp::Dsp;
use crate::exi::Exi;

pub const PHYS_BASE: u32 = 0x0C00_0000;
const SPAN: usize = 0x9000;

const DI_START: u32 = 0x6000;
const DI_END: u32 = 0x6100;
const DSP_START: u32 = 0x5000;
const DSP_END: u32 = 0x5100;
const EXI_START: u32 = 0x6800;
const EXI_END: u32 = 0x6C00;
const FIFO_START: u32 = 0x8000;
const PI_CHIPID: u32 = 0x3024;

const LOCKED_BASE: u32 = 0xE000_0000;
const LOCKED_SIZE: usize = 0x4000;

pub struct Hw {
    mem: Vec<u8>,
    exi: Exi,
    dsp: Dsp,
    di: Di,
    /// Locked cache, mapped at 0xE0000000 (16 KiB).
    locked: Vec<u8>,
    pub trace: bool,
    /// `FEPOR_HW_LIMIT=n`: stop with a crash report after n register accesses (finds polling loops).
    limit: Option<u64>,
    accesses: u64,
    /// Bytes written to the write-gather pipe (0xCC008000): the GX command stream (M3).
    pub fifo_bytes: u64,
}

fn block_name(off: u32) -> &'static str {
    match off >> 8 {
        0x00..=0x0F => "CP",
        0x10..=0x1F => "PE",
        0x20..=0x2F => "VI",
        0x30..=0x3F => "PI",
        0x40..=0x4F => "MI",
        0x50..=0x5F => "DSP",
        0x60..=0x63 => "DI",
        0x64..=0x67 => "SI",
        0x68..=0x6B => "EXI",
        0x6C..=0x6F => "AI",
        0x80..=0x8F => "GXFIFO",
        _ => "?",
    }
}

impl Hw {
    pub fn new() -> Hw {
        Hw {
            mem: vec![0; SPAN],
            exi: Exi::new(),
            dsp: Dsp::new(),
            di: Di::new(),
            locked: vec![0; LOCKED_SIZE],
            trace: std::env::var("FEPOR_TRACE").is_ok_and(|v| v.split(',').any(|x| x == "hw" || x == "all")),
            limit: std::env::var("FEPOR_HW_LIMIT").ok().and_then(|v| v.parse().ok()),
            accesses: 0,
            fifo_bytes: 0,
        }
    }

    /// Attach the player's disc to the DVD interface.
    pub fn attach_disc(&mut self, disc: Box<dyn fepor_core::Disc>) {
        self.di.attach(disc);
    }

    /// Is `ea` in the locked cache?
    fn in_locked(ea: u32) -> bool {
        (LOCKED_BASE..LOCKED_BASE + LOCKED_SIZE as u32).contains(&ea)
    }

    /// Is `ea` a hardware register address?
    pub fn contains(ea: u32) -> bool {
        let p = ea & 0x3FFF_FFFF;
        (PHYS_BASE..PHYS_BASE + SPAN as u32).contains(&p) || Self::in_locked(ea)
    }

    fn offset(ea: u32) -> u32 {
        (ea & 0x3FFF_FFFF) - PHYS_BASE
    }

    fn count(&mut self) -> Result<(), String> {
        self.accesses += 1;
        match self.limit {
            Some(n) if self.accesses > n => Err(format!("hardware access limit ({n}) reached: probable polling loop")),
            _ => Ok(()),
        }
    }

    pub fn read(&mut self, ea: u32, size: u32) -> Result<u32, String> {
        self.count()?;
        if !Self::contains(ea) {
            return Err(format!("read of unmapped address {ea:#010x} ({size} bytes)"));
        }
        if Self::in_locked(ea) {
            let (o, n) = ((ea - LOCKED_BASE) as usize, size as usize);
            return Ok(self.locked[o..o + n].iter().fold(0u32, |a, &b| a << 8 | b as u32));
        }
        let off = Self::offset(ea);
        let v = if (EXI_START..EXI_END).contains(&off) {
            self.exi.read(off - EXI_START, size)
        } else if (DSP_START..DSP_END).contains(&off) {
            self.dsp.read(off - DSP_START, size)
        } else if (DI_START..DI_END).contains(&off) {
            self.di.read(off - DI_START, size)
        } else if off == PI_CHIPID {
            0x2465_00B1
        } else {
            let (o, n) = (off as usize, size as usize);
            self.mem[o..o + n].iter().fold(0u32, |a, &b| a << 8 | b as u32)
        };
        if self.trace {
            eprintln!("hw read{} {ea:#010x} [{}] -> {v:#x}", size * 8, block_name(off));
        }
        Ok(v)
    }

    /// `ram` is the guest RAM, for devices that DMA.
    pub fn write(&mut self, ea: u32, value: u32, size: u32, ram: &mut [u8]) -> Result<(), String> {
        self.count()?;
        if !Self::contains(ea) {
            return Err(format!("write of {value:#x} to unmapped address {ea:#010x} ({size} bytes)"));
        }
        if Self::in_locked(ea) {
            let (o, n) = ((ea - LOCKED_BASE) as usize, size as usize);
            for i in 0..n {
                self.locked[o + i] = (value >> (8 * (n - 1 - i))) as u8;
            }
            return Ok(());
        }
        let off = Self::offset(ea);
        if self.trace {
            eprintln!("hw write{} {ea:#010x} [{}] <- {value:#x}", size * 8, block_name(off));
        }
        if off >= FIFO_START {
            self.fifo_bytes += size as u64;
        } else if (EXI_START..EXI_END).contains(&off) {
            self.exi.write(off - EXI_START, value, ram);
        } else if (DSP_START..DSP_END).contains(&off) {
            self.dsp.write(off - DSP_START, value, size, ram);
        } else if (DI_START..DI_END).contains(&off) {
            self.di.write(off - DI_START, value, ram);
        } else {
            let (o, n) = (off as usize, size as usize);
            for i in 0..n {
                self.mem[o + i] = (value >> (8 * (n - 1 - i))) as u8;
            }
        }
        Ok(())
    }
}

impl Default for Hw {
    fn default() -> Self {
        Hw::new()
    }
}
