//! Hardware registers at 0xCC000000 (physical 0x0C000000..0x0C009000).
//!
//! Registers start as plain storage (what the SDK writes it can read back) and get dedicated
//! behaviour as boot needs it (EXI so far). Every access can be traced with `FEPOR_TRACE=hw`.

use std::time::{Duration, Instant};

use crate::ai::Ai;
use crate::di::Di;
use crate::dsp::Dsp;
use crate::exi::Exi;
use fepor_gx::{Event, Gx, Sink};

pub const PHYS_BASE: u32 = 0x0C00_0000;
const SPAN: usize = 0x9000;

const AI_START: u32 = 0x6C00;
const AI_END: u32 = 0x6D00;
const DI_START: u32 = 0x6000;
const DI_END: u32 = 0x6100;
const DSP_START: u32 = 0x5000;
const DSP_END: u32 = 0x5100;
const EXI_START: u32 = 0x6800;
const EXI_END: u32 = 0x6C00;
const FIFO_START: u32 = 0x8000;
const PI_CHIPID: u32 = 0x3024;
const PI_INTSR: u32 = 0x3000;
const PI_INTMSK: u32 = 0x3004;
/// VI display interrupt registers DI0..DI3.
const VI_DI: u32 = 0x2030;
const VI_DI_END: u32 = 0x2040;
const VI_INT_STATUS: u32 = 1 << 31;
const VI_INT_ENABLE: u32 = 1 << 28;
/// Pixel engine: interrupt status/mask (enables: bit0 token, bit1 finish; flags: bit2 token, bit3 finish, cleared by writing 1) and token value.
const PE_INTSR: u32 = 0x100A;
const PE_TOKEN: u32 = 0x100E;

const LOCKED_BASE: u32 = 0xE000_0000;
const LOCKED_SIZE: usize = 0x4000;

pub struct Hw {
    mem: Vec<u8>,
    exi: Exi,
    dsp: Dsp,
    di: Di,
    ai: Ai,
    vi_di: [u32; 4],
    vi_next: Instant,
    vi_period: Duration,
    /// Retraces so far (`FEPOR_TRACE=vi` prints them).
    pub retraces: u64,
    /// Locked cache, mapped at 0xE0000000 (16 KiB).
    locked: Vec<u8>,
    pub trace: bool,
    trace_vi: bool,
    /// `FEPOR_HW_LIMIT=n`: stop with a crash report after n register accesses (finds polling loops).
    limit: Option<u64>,
    accesses: u64,
    /// The graphics pipeline fed by the write-gather pipe (0xCC008000).
    pub gx: Gx,
    fifo: Vec<u8>,
    sink: Box<dyn Sink>,
    pe_intsr: u16,
    pe_token: u16,
    /// A finish was reached while the token flag was still set; the SDK's token handler would clear
    /// both flags at once, so the finish is raised only after the token interrupt was acknowledged.
    pe_finish_pending: bool,
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
            ai: Ai::new(),
            vi_di: [0; 4],
            vi_next: Instant::now(),
            vi_period: Duration::from_micros(16_683),
            retraces: 0,
            locked: vec![0; LOCKED_SIZE],
            trace: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "hw" || x == "all")),
            trace_vi: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "vi" || x == "all")),
            limit: std::env::var("FEPOR_HW_LIMIT")
                .ok()
                .and_then(|v| v.parse().ok()),
            accesses: 0,
            gx: Gx::new(),
            fifo: Vec::new(),
            sink: Box::new(CountingSink::default()),
            pe_intsr: 0,
            pe_token: 0,
            pe_finish_pending: false,
        }
    }

    /// 50 Hz for PAL, 59.94 Hz otherwise.
    pub fn set_video_mode(&mut self, pal: bool) {
        self.vi_period = Duration::from_micros(if pal { 20_000 } else { 16_683 });
    }

    /// Advance the video timing: raises the retrace interrupt flags.
    fn vi_tick(&mut self) {
        let now = Instant::now();
        while now >= self.vi_next {
            self.vi_next += self.vi_period;
            self.retraces += 1;
            for di in &mut self.vi_di {
                if *di & VI_INT_ENABLE != 0 {
                    *di |= VI_INT_STATUS;
                }
            }
            if self.trace_vi && self.retraces % 60 == 0 {
                eprintln!("vi: {} retraces", self.retraces);
            }
        }
    }

    /// Interrupt cause bits as the PI reports them.
    fn pi_cause(&mut self) -> u32 {
        self.vi_tick();
        self.dsp.tick();
        if self.pe_finish_pending && self.pe_intsr & 4 == 0 {
            self.pe_finish_pending = false;
            self.pe_intsr |= 8;
        }
        let mut cause = 0;
        if self.di.interrupt_pending() {
            cause |= 1 << 2;
        }
        if self.dsp.interrupt_pending() {
            cause |= 1 << 6;
        }
        if self
            .vi_di
            .iter()
            .any(|d| d & VI_INT_STATUS != 0 && d & VI_INT_ENABLE != 0)
        {
            cause |= 1 << 8;
        }
        if self.pe_intsr & 0b0101 == 0b0101 {
            cause |= 1 << 9;
        }
        if self.pe_intsr & 0b1010 == 0b1010 {
            cause |= 1 << 10;
        }
        cause
    }

    /// Is an unmasked interrupt waiting for the CPU?
    pub fn interrupt_pending(&mut self) -> bool {
        let mask = u32::from_be_bytes(
            self.mem[PI_INTMSK as usize..PI_INTMSK as usize + 4]
                .try_into()
                .unwrap(),
        );
        self.pi_cause() & mask != 0
    }

    /// Run the command processor on everything gathered so far.
    pub fn flush_fifo(&mut self, ram: &[u8]) {
        let bytes = std::mem::take(&mut self.fifo);
        let mut tap = Tap {
            inner: &mut *self.sink,
            finish: false,
            token: None,
        };
        self.gx.feed(&bytes, ram, &mut tap);
        let (finish, token) = (tap.finish, tap.token);
        if let Some(t) = token {
            self.pe_token = t;
            self.pe_intsr |= 4;
        }
        self.pe_finish_pending |= finish;
    }

    /// Replace the default (counting) video backend.
    pub fn set_sink(&mut self, sink: Box<dyn Sink>) {
        self.sink = sink;
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
            Some(n) if self.accesses > n => Err(format!(
                "hardware access limit ({n}) reached: probable polling loop"
            )),
            _ => Ok(()),
        }
    }

    pub fn read(&mut self, ea: u32, size: u32) -> Result<u32, String> {
        self.count()?;
        if !Self::contains(ea) {
            return Err(format!(
                "read of unmapped address {ea:#010x} ({size} bytes)"
            ));
        }
        if Self::in_locked(ea) {
            let (o, n) = ((ea - LOCKED_BASE) as usize, size as usize);
            return Ok(self.locked[o..o + n]
                .iter()
                .fold(0u32, |a, &b| a << 8 | b as u32));
        }
        let off = Self::offset(ea);
        let v = if (EXI_START..EXI_END).contains(&off) {
            self.exi.read(off - EXI_START, size)
        } else if (DSP_START..DSP_END).contains(&off) {
            self.dsp.read(off - DSP_START, size)
        } else if (DI_START..DI_END).contains(&off) {
            self.di.read(off - DI_START, size)
        } else if (AI_START..AI_END).contains(&off) && size == 4 {
            self.ai.read(off - AI_START)
        } else if off == PE_INTSR && size == 2 {
            self.pe_intsr as u32
        } else if off == PE_TOKEN && size == 2 {
            self.pe_token as u32
        } else if off == PI_CHIPID {
            0x2465_00B1
        } else if off == PI_INTSR {
            self.pi_cause()
        } else if (VI_DI..VI_DI_END).contains(&off) {
            let w = self.vi_di[((off - VI_DI) / 4) as usize];
            match (size, off & 2) {
                (4, _) => w,
                (2, 0) => w >> 16,
                _ => w & 0xFFFF,
            }
        } else {
            let (o, n) = (off as usize, size as usize);
            self.mem[o..o + n]
                .iter()
                .fold(0u32, |a, &b| a << 8 | b as u32)
        };
        if self.trace {
            eprintln!(
                "hw read{} {ea:#010x} [{}] -> {v:#x}",
                size * 8,
                block_name(off)
            );
        }
        Ok(v)
    }

    /// `ram` is the guest RAM, for devices that DMA.
    pub fn write(&mut self, ea: u32, value: u32, size: u32, ram: &mut [u8]) -> Result<(), String> {
        self.count()?;
        if !Self::contains(ea) {
            return Err(format!(
                "write of {value:#x} to unmapped address {ea:#010x} ({size} bytes)"
            ));
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
            eprintln!(
                "hw write{} {ea:#010x} [{}] <- {value:#x}",
                size * 8,
                block_name(off)
            );
        }
        if off >= FIFO_START {
            // the write-gather pipe: big-endian bytes, handed to the command processor in 32-byte bursts
            self.fifo
                .extend((0..size).rev().map(|i| (value >> (8 * i)) as u8));
            if self.fifo.len() >= 32 {
                self.flush_fifo(ram);
            }
        } else if (EXI_START..EXI_END).contains(&off) {
            self.exi.write(off - EXI_START, value, ram);
        } else if (DSP_START..DSP_END).contains(&off) {
            self.dsp.write(off - DSP_START, value, size, ram);
        } else if (DI_START..DI_END).contains(&off) {
            self.di.write(off - DI_START, value, ram);
        } else if (AI_START..AI_END).contains(&off) && size == 4 {
            self.ai.write(off - AI_START, value);
        } else if off == PE_INTSR && size == 2 {
            // flags clear when written with 1; enables are storage
            self.pe_intsr =
                (self.pe_intsr & !0b0011 | (value as u16 & 0b0011)) & !(value as u16 & 0b1100);
        } else if (VI_DI..VI_DI_END).contains(&off) {
            // Each DIx is a 32-bit register the SDK writes as two halfwords. The interrupt status bit clears
            // when written with 0; the enable bit and the position are plain storage.
            let di = &mut self.vi_di[((off - VI_DI) / 4) as usize];
            let merged = match (size, off & 2) {
                (4, _) => value,
                (2, 0) => value << 16 | (*di & 0xFFFF),
                _ => (*di & 0xFFFF_0000) | (value & 0xFFFF),
            };
            *di = (merged & !VI_INT_STATUS) | (*di & VI_INT_STATUS & merged);
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

/// Default backend: counts what the game draws.
#[derive(Default)]
pub struct CountingSink {
    frames: u64,
}

impl Sink for CountingSink {
    fn event(&mut self, event: Event, _ram: &[u8], _tluts: &fepor_gx::state::Tluts) {
        if let Event::Copy(c) = &event {
            if c.to_xfb {
                self.frames += 1;
                if std::env::var("FEPOR_TRACE")
                    .is_ok_and(|v| v.split(',').any(|x| x == "gx" || x == "all"))
                {
                    eprintln!("gx: frame {} copied to XFB at {:#x}", self.frames, c.dest);
                }
            }
        }
    }
}

/// Forwards to the real backend while noting the events that raise pixel-engine interrupts.
struct Tap<'a> {
    inner: &'a mut dyn Sink,
    finish: bool,
    token: Option<u16>,
}

impl Sink for Tap<'_> {
    fn event(&mut self, event: Event, ram: &[u8], tluts: &fepor_gx::state::Tluts) {
        match &event {
            Event::Finish => self.finish = true,
            Event::Token(t) => self.token = Some(*t),
            _ => {}
        }
        self.inner.event(event, ram, tluts);
    }
}
