//! Hardware registers at 0xCC000000 (physical 0x0C000000..0x0C009000).
//!
//! Registers start as plain storage (what the SDK writes it can read back) and get dedicated
//! behaviour as boot needs it (EXI so far). Every access can be traced with `FEPOR_TRACE=hw`.

use std::time::Instant;

use crate::ai::Ai;
use crate::di::Di;
use crate::dsp::{AudioSink, Dsp};
use crate::exi::Exi;
use crate::si::Si;
use fepor_core::snap::W;
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
const SI_START: u32 = 0x6400;
const SI_END: u32 = 0x6500;
const PI_CHIPID: u32 = 0x3024;
const PI_INTSR: u32 = 0x3000;
const PI_INTMSK: u32 = 0x3004;
/// VI display interrupt registers DI0..DI3.
const VI_DI: u32 = 0x2030;
/// VI display configuration register (enable in bit 0, video format in bits 8-9).
const VI_DCR: u32 = 0x2002;
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
    pub si: Si,
    vi_di: [u32; 4],
    /// Guest nanoseconds (`clock`) of the next retrace, and the time between retraces.
    vi_next: u64,
    vi_period: u64,
    /// Retraces so far (`FEPOR_TRACE=vi` prints them).
    pub retraces: u64,
    /// Locked cache, mapped at 0xE0000000 (16 KiB).
    locked: Vec<u8>,
    pub trace: bool,
    trace_vi: bool,
    /// Retraces that came due more than a period ago since `vi_steer_at` (real time).
    vi_late: u32,
    vi_steer_at: Instant,
    /// Retraces and game frames at `vi_steer_at`.
    vi_steer_mark: (u64, u64),
    /// (when, retraces, game frames) at the last `FEPOR_TRACE=vi` line.
    trace_vi_last: (Instant, u64, u64),
    /// `FEPOR_HW_LIMIT=n`: stop with a crash report after n register accesses (finds polling loops).
    limit: Option<u64>,
    accesses: u64,
    /// The graphics pipeline fed by the write-gather pipe (0xCC008000).
    pub gx: Gx,
    fifo: Vec<u8>,
    sink: Box<dyn Sink>,
    audio: Option<Box<dyn AudioSink>>,
    pe_intsr: u16,
    trace_gx: bool,
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
            si: Si::new(),
            vi_di: [0; 4],
            vi_next: crate::clock::now_ns(),
            vi_period: 16_683_000,
            retraces: 0,
            locked: vec![0; LOCKED_SIZE],
            trace: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "hw" || x == "all")),
            trace_vi: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "vi" || x == "all")),
            vi_late: 0,
            vi_steer_at: Instant::now(),
            vi_steer_mark: (0, 0),
            trace_vi_last: (Instant::now(), 0, 0),
            limit: std::env::var("FEPOR_HW_LIMIT")
                .ok()
                .and_then(|v| v.parse().ok()),
            accesses: 0,
            gx: Gx::new(),
            fifo: Vec::new(),
            sink: Box::new(CountingSink::default()),
            audio: None,
            pe_intsr: 0,
            trace_gx: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "gxstats")),
            pe_token: 0,
            pe_finish_pending: false,
        }
    }

    /// 50 Hz for PAL, 59.94 Hz otherwise.
    pub fn set_video_mode(&mut self, pal: bool) {
        self.vi_period = if pal { 20_000_000 } else { 16_683_000 };
    }

    /// Advance the video timing: raises the retrace interrupt flags.
    fn vi_tick(&mut self) {
        let now = crate::clock::now_ns();
        // more than one retrace due at once: the guest did not get to run in between (uncapped speed steers by this)
        if now >= self.vi_next + self.vi_period {
            self.vi_late += 1;
        }
        self.vi_steer();
        while now >= self.vi_next {
            self.vi_next += self.vi_period;
            self.retraces += 1;
            fepor_core::stats::RETRACES.store(self.retraces, std::sync::atomic::Ordering::Relaxed);
            self.si.retrace(self.retraces);
            for di in &mut self.vi_di {
                if *di & VI_INT_ENABLE != 0 {
                    *di |= VI_INT_STATUS;
                }
            }
            if self.trace_vi {
                self.trace_vi_line();
            }
        }
    }

    /// Every quarter second of real time tell the uncapped speed whether the guest kept up: it fell behind
    /// when retraces came due while it was busy, or when the game drew fewer than 0.4 frames per retrace
    /// (a game that draws nothing, a loading screen, does not count).
    fn vi_steer(&mut self) {
        if self.vi_steer_at.elapsed() < std::time::Duration::from_millis(250) {
            return;
        }
        let frames = fepor_core::stats::GAME_FRAMES.load(std::sync::atomic::Ordering::Relaxed);
        let (dr, df) = (
            self.retraces - self.vi_steer_mark.0,
            frames - self.vi_steer_mark.1,
        );
        fepor_core::speed::adapt(self.vi_late > 2 || (df > 0 && df * 5 < dr * 2));
        self.vi_late = 0;
        self.vi_steer_at = Instant::now();
        self.vi_steer_mark = (self.retraces, frames);
    }

    /// `FEPOR_TRACE=vi`: once per real second, the retraces and game frames of that second (the game's speed
    /// as the machine sees it) and the speed factor.
    fn trace_vi_line(&mut self) {
        let (at, retraces, frames) = self.trace_vi_last;
        let secs = at.elapsed().as_secs_f64();
        if secs < 1.0 {
            return;
        }
        let game_frames = fepor_core::stats::GAME_FRAMES.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!(
            "vi: {:.1} retraces/s, {:.1} game frames/s (speed x{}); {} retraces, {} thread switches",
            (self.retraces - retraces) as f64 / secs,
            (game_frames - frames) as f64 / secs,
            fepor_core::speed::factor(),
            self.retraces,
            crate::sched::switches()
        );
        self.trace_vi_last = (Instant::now(), self.retraces, game_frames);
    }

    /// Interrupt cause bits as the PI reports them.
    fn pi_cause(&mut self) -> u32 {
        self.vi_tick();
        self.sink.tick();
        self.dsp.tick(self.ai.rate() as u32);
        if self.pe_finish_pending && self.pe_intsr & 4 == 0 {
            self.pe_finish_pending = false;
            self.pe_intsr |= 8;
        }
        let mut cause = 0;
        if self.di.interrupt_pending() {
            cause |= 1 << 2;
        }
        if self.si.interrupt_pending() {
            cause |= 1 << 3;
        }
        if self.exi.interrupt_pending() {
            cause |= 1 << 4;
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
        let mut tap = Tap {
            inner: &mut *self.sink,
            finish: false,
            token: None,
        };
        self.gx.feed(&self.fifo, ram, &mut tap);
        // the buffer keeps its capacity: this runs for every 32 bytes the game sends
        self.fifo.clear();
        let (finish, token) = (tap.finish, tap.token);
        if self.trace_gx && finish {
            eprintln!("gx: draw done; parser so far: {:?}", self.gx.stats);
        }
        if let Some(t) = token {
            self.pe_token = t;
            self.pe_intsr |= 4;
        }
        self.pe_finish_pending |= finish;
    }

    /// Locked-cache DMA (`mtspr DMA_L` with the trigger bit): `blocks` 32-byte blocks between main memory
    /// and the locked cache; `load` copies memory into the cache.
    pub fn lc_dma(&mut self, mem: u32, lc: u32, blocks: usize, load: bool, ram: &mut [u8]) {
        let (m, l) = ((mem & 0x03FF_FFFF) as usize, (lc & 0x3FFF) as usize);
        let n = (blocks * 32)
            .min(LOCKED_SIZE - l)
            .min(ram.len().saturating_sub(m));
        if load {
            self.locked[l..l + n].copy_from_slice(&ram[m..m + n]);
        } else {
            ram[m..m + n].copy_from_slice(&self.locked[l..l + n]);
        }
    }

    /// Replace the default (counting) video backend.
    pub fn set_sink(&mut self, sink: Box<dyn Sink>) {
        self.sink = sink;
    }

    /// Send the game's audio (AI DMA) to `sink`.
    pub fn set_audio_sink(&mut self, sink: Box<dyn AudioSink>) {
        self.audio = Some(sink);
    }

    /// Advance the audio DMA clock and hand finished buffers to the audio sink. Called from safe points,
    /// before an AI interrupt can let the guest refill a buffer.
    pub fn pump_audio(&mut self, ram: &[u8]) {
        self.dsp.tick(self.ai.rate() as u32);
        match self.audio.as_mut() {
            Some(s) => self.dsp.drain_audio(ram, s.as_mut()),
            None => self.dsp.drain_audio(ram, &mut NullSink),
        }
    }

    /// Set the console language the game reads from the SRAM (`OS_LANG_*`).
    pub fn set_language(&mut self, code: u8) {
        self.exi.set_language(code);
    }

    /// Insert a memory card in slot A (0) or B (1).
    pub fn insert_card(&mut self, slot: usize, card: crate::card::MemCard) {
        self.exi.insert_card(slot, card);
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
        } else if (SI_START..SI_END).contains(&off) {
            self.si.read(off - SI_START, size)
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
        if off == VI_DCR && size == 2 && value & 1 != 0 {
            // DCR.FMT: 1 = PAL timing (50 Hz); NTSC, MPAL and EURGB60 all run at 59.94 Hz
            self.set_video_mode(value >> 8 & 3 == 1);
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
        } else if (SI_START..SI_END).contains(&off) {
            self.si.write(off - SI_START, value, size);
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

/// Drops audio when no output device is attached.
struct NullSink;

impl AudioSink for NullSink {
    fn push(&mut self, _rate: u32, _samples: &[i16]) {}
}

impl Hw {
    /// A hash of the video backend's picture, for the state trace.
    pub fn efb_hash(&mut self) -> Option<u64> {
        self.sink.picture_hash()
    }

    /// Write the hardware state (save states, ADR 0035). Times are written relative to now.
    pub fn save_state(&mut self, w: &mut fepor_core::snap::W) {
        w.bytes(&self.mem);
        w.bytes(&self.locked);
        w.u32s(&self.vi_di);
        w.u64(crate::state::until(self.vi_next));
        w.u64(self.retraces);
        self.exi.save(w);
        self.dsp.save(w);
        self.di.save(w);
        self.ai.save(w);
        self.si.save(w);
        self.gx.save_state(w);
        w.bytes(&self.fifo);
        w.u16(self.pe_intsr);
        w.u16(self.pe_token);
        w.bool(self.pe_finish_pending);
        let mut sink = W::new();
        self.sink.save_state(&mut sink);
        w.bytes(&sink.buf);
    }

    pub fn load_state(&mut self, r: &mut fepor_core::snap::R, ram: &[u8]) {
        r.bytes_into(&mut self.mem);
        r.bytes_into(&mut self.locked);
        r.u32s_into(&mut self.vi_di);
        self.vi_next = crate::state::from_now(r.u64());
        self.retraces = r.u64();
        self.exi.load(r);
        self.dsp.load(r);
        self.di.load(r);
        self.ai.load(r);
        self.si.load(r);
        self.gx.load_state(r);
        self.fifo = r.bytes().to_vec();
        self.pe_intsr = r.u16();
        self.pe_token = r.u16();
        self.pe_finish_pending = r.bool();
        let sink = r.bytes();
        let mut sr = fepor_core::snap::R::new(sink);
        self.sink.load_state(&mut sr, ram);
        if sr.failed {
            r.failed = true;
        }
        if let Some(a) = self.audio.as_mut() {
            a.discard();
        }
    }
}
