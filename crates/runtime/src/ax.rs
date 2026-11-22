//! High-level emulation of the AX audio microcode (the GameCube SDK's voice mixer).
//!
//! The game's DSP ucode (identified by its Ector hash, 0x3ad3b7ac for GFEP01) renders 5 ms of audio per
//! command list: the CPU sends `0xBABE0000 | size`, then the command list address; the ucode walks the list
//! (voice parameter blocks, mixing, aux sends, output) and answers `DSP_YIELD`. This module reproduces that
//! behaviour on guest RAM and ARAM. Structure and arithmetic follow Dolphin's DSPHLE AX implementation
//! (GPL-2.0-or-later) so results match it sample for sample, except that the DROM polyphase coefficients
//! are not available here: polyphase voices use linear interpolation, as Dolphin does without `dsp_coef.bin`.

const FRAME: usize = 5 * 32;
const PB_WORDS: usize = 122;

pub const MAIL_CMDLIST: u32 = 0xBABE_0000;
const MAIL_CMDLIST_MASK: u32 = 0xFFFF_0000;
const TASK_MAIL_MASK: u32 = 0xFFFF_0000;
const TASK_MAIL_TO_DSP: u32 = 0xCDD1_0000;
const MAIL_RESUME: u32 = 0xCDD1_0000;
const MAIL_NEW_UCODE: u32 = 0xCDD1_0001;
const MAIL_RESET: u32 = 0xCDD1_0002;
const MAIL_CONTINUE: u32 = 0xCDD1_0003;
pub const DSP_RESUME: u32 = 0xDCD1_0001;
pub const DSP_YIELD: u32 = 0xDCD1_0002;

// buffer indices
const MAIN_L: usize = 0;
const MAIN_R: usize = 1;
const MAIN_S: usize = 2;
const AUXA_L: usize = 3;
const AUXB_L: usize = 6;

// mixer control bits (AXMixControl)
const MIX_MAIN_L: u32 = 0x000001;
const MIX_MAIN_L_RAMP: u32 = 0x000002;
const MIX_MAIN_R: u32 = 0x000004;
const MIX_MAIN_R_RAMP: u32 = 0x000008;
const MIX_MAIN_S: u32 = 0x000010;
const MIX_MAIN_S_RAMP: u32 = 0x000020;
const MIX_AUXA_L: u32 = 0x000040;
const MIX_AUXA_L_RAMP: u32 = 0x000080;
const MIX_AUXA_R: u32 = 0x000100;
const MIX_AUXA_R_RAMP: u32 = 0x000200;
const MIX_AUXA_S: u32 = 0x000400;
const MIX_AUXA_S_RAMP: u32 = 0x000800;
const MIX_AUXB_L: u32 = 0x001000;
const MIX_AUXB_L_RAMP: u32 = 0x002000;
const MIX_AUXB_R: u32 = 0x004000;
const MIX_AUXB_R_RAMP: u32 = 0x008000;
const MIX_AUXB_S: u32 = 0x010000;
const MIX_AUXB_S_RAMP: u32 = 0x020000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MailState {
    WaitingForCmdListSize,
    WaitingForCmdListAddress,
    WaitingForNextTask,
}

/// Guest memory the ucode works on.
pub struct Mem<'a> {
    pub ram: &'a mut [u8],
    pub aram: &'a [u8],
}

impl Mem<'_> {
    fn phys(addr: u32) -> usize {
        (addr & 0x03FF_FFFF) as usize
    }
    fn r16(&self, addr: u32) -> u16 {
        let a = Self::phys(addr);
        self.ram
            .get(a..a + 2)
            .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn r32(&self, addr: u32) -> u32 {
        let a = Self::phys(addr);
        self.ram
            .get(a..a + 4)
            .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn w16(&mut self, addr: u32, v: u16) {
        let a = Self::phys(addr);
        if let Some(b) = self.ram.get_mut(a..a + 2) {
            b.copy_from_slice(&v.to_be_bytes());
        }
    }
    fn w32(&mut self, addr: u32, v: u32) {
        let a = Self::phys(addr);
        if let Some(b) = self.ram.get_mut(a..a + 4) {
            b.copy_from_slice(&v.to_be_bytes());
        }
    }
    fn aram(&self, addr: u32) -> u8 {
        self.aram
            .get(addr as usize & (self.aram.len().max(1) - 1))
            .copied()
            .unwrap_or(0)
    }
}

fn hilo(hi: u16, lo: u16) -> u32 {
    (hi as u32) << 16 | lo as u32
}

fn clamp16(v: i64) -> i16 {
    v.clamp(-0x8000, 0x7FFF) as i16
}

/// The DSP accelerator: reads and decodes voice samples from ARAM.
struct Accel {
    start: u32,
    end: u32,
    cur: u32,
    format: u16,
    gain: i16,
    yn1: i16,
    yn2: i16,
    pred_scale: u16,
    reads_stopped: bool,
}

impl Accel {
    fn size(&self) -> u16 {
        self.format & 3
    }
    fn decode(&self) -> u16 {
        self.format >> 2 & 3
    }
    fn gain_scale(&self) -> u16 {
        self.format >> 4 & 3
    }

    fn current_sample(&self, mem: &Mem) -> u16 {
        match self.size() {
            0 => {
                let v = mem.aram(self.cur >> 1) as u16;
                if self.cur & 1 != 0 {
                    v & 0xF
                } else {
                    v >> 4
                }
            }
            1 => mem.aram(self.cur) as u16,
            2 => {
                (mem.aram(self.cur.wrapping_mul(2)) as u16) << 8
                    | mem.aram(self.cur.wrapping_mul(2).wrapping_add(1)) as u16
            }
            _ => 0,
        }
    }

    /// One sample; may hit the end of the voice (returns the sample, flags handled through `pb`).
    fn read_sample(&mut self, pb: &mut [u16; PB_WORDS], mem: &Mem) -> i16 {
        if self.reads_stopped {
            return 0;
        }
        let raw = self.current_sample(mem) as i16;
        let coef_idx = ((self.pred_scale >> 4) & 7) as usize;
        let coef1 = pb[63 + coef_idx * 2] as i16 as i32;
        let coef2 = pb[63 + coef_idx * 2 + 1] as i16 as i32;
        let val: i16;
        let step: u32;
        match self.decode() {
            0 => {
                let mut r = (raw & 0xF) as i32;
                let scale = 1i32 << (self.pred_scale & 0xF);
                if r >= 8 {
                    r -= 16;
                }
                let v32 =
                    scale * r + ((0x400 + coef1 * self.yn1 as i32 + coef2 * self.yn2 as i32) >> 11);
                val = v32.clamp(-0x8000, 0x7FFF) as i16;
                step = 2;
                self.yn2 = self.yn1;
                self.yn1 = val;
                self.cur = self.cur.wrapping_add(1);
                if self.end & 0xF == 0 && self.cur == self.end {
                    self.cur = self.start + 1;
                } else if self.end & 0xF == 1 && self.cur == self.end.wrapping_sub(1) {
                    self.cur = self.start;
                } else if self.cur & 15 == 0 {
                    self.pred_scale = mem.aram((self.cur & !15) >> 1) as u16;
                    self.cur += 2;
                    return self.finish(val, step + 2, pb);
                }
            }
            _ => {
                let shift = match self.gain_scale() {
                    0 => 11,
                    1 => 0,
                    2 => 16,
                    _ => 0,
                };
                let v32 = ((self.gain as i32 * raw as i32) >> shift)
                    + (((coef1 * self.yn1 as i32) >> shift) + ((coef2 * self.yn2 as i32) >> shift));
                val = v32 as i16;
                self.yn2 = self.yn1;
                self.yn1 = val;
                step = 2;
                if self.decode() != 1 {
                    self.cur = self.cur.wrapping_add(1);
                }
            }
        }
        self.finish(val, step, pb)
    }

    fn finish(&mut self, val: i16, step: u32, pb: &mut [u16; PB_WORDS]) -> i16 {
        if self.cur == self.end.wrapping_add(step).wrapping_sub(1) {
            self.cur = self.start;
            self.reads_stopped = true;
            self.end_exception(pb);
        }
        val
    }

    fn end_exception(&mut self, pb: &mut [u16; PB_WORDS]) {
        if pb[55] != 0 {
            // looping: continue from the loop point
            self.pred_scale = pb[90] & 0x7F;
            if pb[8] != 1 {
                self.yn1 = pb[91] as i16;
                self.yn2 = pb[92] as i16;
                self.reads_stopped = false;
            } else {
                // streams refresh yn1/yn2 (which resumes reads) and count loops
                self.reads_stopped = false;
                pb[97] = pb[97].wrapping_add(1);
            }
        } else {
            pb[7] = 0; // a one-shot voice reached its end
        }
    }
}

/// Resample `count` samples (`input` yields the source samples), returns the new fractional position.
fn resample(
    mut input: impl FnMut() -> i16,
    out: &mut [i16],
    count: usize,
    last: &mut [i16; 4],
    mut pos: u32,
    ratio: u32,
    srctype: u16,
) -> u32 {
    if srctype == 0 || srctype == 1 {
        let mut temp = [0i16; 4];
        let mut idx: usize = 0;
        for &s in last.iter() {
            temp[idx & 3] = s;
            idx += 1;
        }
        for o in out.iter_mut().take(count) {
            pos = pos.wrapping_add(ratio);
            while pos >= 0x10000 {
                temp[idx & 3] = input();
                idx += 1;
                pos -= 0x10000;
            }
            let frac = (pos & 0xFFFF) as u16;
            let inv = frac.wrapping_neg();
            let sample = if frac != 0 {
                let s0 = temp[idx & 3] as i32;
                idx += 1;
                let s1 = temp[idx & 3] as i32;
                idx += 1;
                idx += 2;
                (((s0 * inv as i32) + (s1 * frac as i32)) >> 16) as i16
            } else {
                let s = temp[idx & 3];
                idx += 1;
                idx += 3;
                s
            };
            *o = sample;
        }
        idx -= 1;
        last[3] = temp[idx & 3];
        idx -= 1;
        last[2] = temp[idx & 3];
        idx -= 1;
        last[1] = temp[idx & 3];
        idx -= 1;
        last[0] = temp[idx & 3];
    } else {
        for o in out.iter_mut().take(count) {
            *o = input();
        }
        let n = count.max(4);
        let tail = &out[n - 4..n];
        last.copy_from_slice(tail);
    }
    pos
}

fn mix_add(
    out: &mut [i32],
    input: &[i16],
    count: usize,
    pb: &mut [u16; PB_WORDS],
    vol_at: usize,
    dpop_at: usize,
    ramp: bool,
) {
    let mut volume = pb[vol_at];
    let delta = if ramp { pb[vol_at + 1] } else { 0 };
    for i in 0..count {
        let s = (input[i] as i64 * volume as i64) >> 15;
        let s16 = clamp16(s as i32 as i64);
        out[i] += s16 as i32;
        volume = volume.wrapping_add(delta);
        pb[dpop_at] = s16 as u16;
    }
    pb[vol_at] = volume;
}

fn convert_mixer_control(m: u32) -> u32 {
    let mut r = 0;
    let set = |cond: bool, bits: u32, r: &mut u32| {
        if cond {
            *r |= bits;
        }
    };
    set(m & 0x0001 != 0, MIX_MAIN_L, &mut r);
    set(m & 0x0002 != 0, MIX_MAIN_R, &mut r);
    set(m & 0x0004 != 0, MIX_MAIN_S, &mut r);
    set(
        m & 0x0008 != 0,
        MIX_MAIN_L_RAMP | MIX_MAIN_R_RAMP | MIX_MAIN_S_RAMP,
        &mut r,
    );
    set(m & 0x0010 != 0, MIX_AUXA_L, &mut r);
    set(m & 0x0020 != 0, MIX_AUXA_R, &mut r);
    set(m & 0x0040 != 0, MIX_AUXA_L_RAMP | MIX_AUXA_R_RAMP, &mut r);
    set(m & 0x0080 != 0, MIX_AUXA_S, &mut r);
    set(m & 0x0100 != 0, MIX_AUXA_S_RAMP, &mut r);
    set(m & 0x0200 != 0, MIX_AUXB_L, &mut r);
    set(m & 0x0400 != 0, MIX_AUXB_R, &mut r);
    set(m & 0x0800 != 0, MIX_AUXB_L_RAMP | MIX_AUXB_R_RAMP, &mut r);
    set(m & 0x1000 != 0, MIX_AUXB_S, &mut r);
    set(m & 0x2000 != 0, MIX_AUXB_S_RAMP, &mut r);
    r
}

pub struct Ax {
    state: MailState,
    cmd_size: usize,
    cmdlist: [u16; 512],
    /// main L/R/S, auxA L/R/S, auxB L/R/S, 5 ms each
    bufs: [[i32; FRAME]; 9],
    compressor_pos: u16,
    /// Mails left to swallow while the CPU uploads a new ucode.
    upload_mails: u32,
    pub frames: u64,
    pub unknown_cmds: u64,
    /// The CPU asked the ucode to hand the DSP back to the ROM.
    pub reset_requested: bool,
    /// `FEPOR_TRACE=ax`: describe every running voice once in a while.
    trace: bool,
    /// `FEPOR_AX_EVERY=n`: trace every n-th frame (default 20; 1 = every frame).
    trace_every: u64,
    voices_run: u64,
}

impl Default for Ax {
    fn default() -> Self {
        Ax::new()
    }
}

impl Ax {
    pub fn new() -> Ax {
        Ax {
            state: MailState::WaitingForCmdListSize,
            cmd_size: 0,
            cmdlist: [0; 512],
            bufs: [[0; FRAME]; 9],
            compressor_pos: 0,
            upload_mails: 0,
            frames: 0,
            unknown_cmds: 0,
            reset_requested: false,
            trace: std::env::var("FEPOR_TRACE")
                .is_ok_and(|v| v.split(',').any(|x| x == "ax" || x == "all")),
            trace_every: std::env::var("FEPOR_AX_EVERY")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(20),
            voices_run: 0,
        }
    }

    /// Handle one mail from the CPU; returns the mail the ucode answers with, if any.
    pub fn handle_mail(&mut self, mail: u32, mem: &mut Mem) -> Option<u32> {
        if self.upload_mails > 0 {
            self.upload_mails -= 1;
            return if self.upload_mails == 0 {
                Some(DSP_RESUME)
            } else {
                None
            };
        }
        match self.state {
            MailState::WaitingForCmdListSize => {
                if mail & MAIL_CMDLIST_MASK == MAIL_CMDLIST {
                    self.cmd_size = (mail & 0xFFFF) as usize;
                    self.state = MailState::WaitingForCmdListAddress;
                }
                None
            }
            MailState::WaitingForCmdListAddress => {
                self.copy_cmdlist(mail, self.cmd_size, mem);
                self.run_cmdlist(mem);
                self.frames += 1;
                if self.trace && self.frames % 200 == 0 {
                    eprintln!(
                        "ax: frame {} voices run {} (cmdlist {} words)",
                        self.frames, self.voices_run, self.cmd_size
                    );
                    eprintln!(
                        "ax: cmdlist {:04x?}",
                        &self.cmdlist[..self.cmd_size.min(512)]
                    );
                    self.voices_run = 0;
                }
                self.state = MailState::WaitingForNextTask;
                Some(DSP_YIELD)
            }
            MailState::WaitingForNextTask => {
                let mail = if mail & TASK_MAIL_MASK != TASK_MAIL_TO_DSP {
                    TASK_MAIL_TO_DSP | (mail & !TASK_MAIL_MASK)
                } else {
                    mail
                };
                match mail {
                    MAIL_RESUME => {
                        self.state = MailState::WaitingForCmdListSize;
                        Some(DSP_RESUME)
                    }
                    MAIL_NEW_UCODE => {
                        // ten mails describe the next ucode; the game only swaps to ucodes we do not model
                        self.upload_mails = 10;
                        self.state = MailState::WaitingForCmdListSize;
                        None
                    }
                    MAIL_RESET => {
                        self.reset_requested = true;
                        None
                    }
                    MAIL_CONTINUE => {
                        self.state = MailState::WaitingForCmdListSize;
                        None
                    }
                    _ => None,
                }
            }
        }
    }

    fn copy_cmdlist(&mut self, addr: u32, size: usize, mem: &Mem) {
        if size >= self.cmdlist.len() {
            return;
        }
        for i in 0..size {
            self.cmdlist[i] = mem.r16(addr + 2 * i as u32);
        }
    }

    fn run_cmdlist(&mut self, mem: &mut Mem) {
        let mut idx = 0usize;
        let mut pb_addr = 0u32;
        loop {
            if idx >= self.cmdlist.len() {
                break;
            }
            let cmd = self.cmdlist[idx];
            idx += 1;
            let arg = |s: &Self, n: &mut usize| {
                let v = s.cmdlist[*n % 512];
                *n += 1;
                v
            };
            match cmd {
                0x00 => {
                    let a = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    self.setup(a, mem);
                }
                0x01 => {
                    let a = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    let (vm, va, vb) = (
                        arg(self, &mut idx),
                        arg(self, &mut idx),
                        arg(self, &mut idx),
                    );
                    self.download_and_mix(a, vm, va, vb, mem);
                }
                0x02 => pb_addr = hilo(arg(self, &mut idx), arg(self, &mut idx)),
                0x03 => self.process_pb_list(pb_addr, mem),
                0x04 | 0x05 => {
                    let a = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    let b = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    self.mix_aux((cmd - 4) as usize, a, b, mem);
                }
                0x06 => {
                    let a = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    self.upload_lrs(a, mem);
                }
                0x07 => {
                    let a = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    self.set_main_lr(a, mem);
                }
                0x08 => idx += 10,
                0x09 => {
                    let b = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    self.mix_aux(1, 0, b, mem);
                }
                0x0A..=0x0C => {}
                0x0D => {
                    let a = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    let size = arg(self, &mut idx) as usize;
                    self.copy_cmdlist(a, size, mem);
                    idx = 0;
                }
                0x0E => {
                    let surround = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    let lr = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    self.output(lr, surround, mem);
                }
                0x0F => break,
                0x10 => {
                    let a = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    let b = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    self.mix_auxb_lr(a, b, mem);
                }
                0x11 => {
                    let a = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    self.set_opposite_lr(a, mem);
                }
                0x12 => {
                    let threshold = arg(self, &mut idx);
                    let frames = arg(self, &mut idx);
                    let a = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    self.compressor(threshold, frames, a, mem);
                }
                0x13 => {
                    let mut v = [0u32; 6];
                    for x in v.iter_mut() {
                        *x = hilo(arg(self, &mut idx), arg(self, &mut idx));
                    }
                    self.send_aux_and_mix(v, mem);
                }
                _ => {
                    self.unknown_cmds += 1;
                    if self.trace {
                        eprintln!("ax: unknown command {cmd:#x} at word {}", idx - 1);
                    }
                    break;
                }
            }
        }
    }

    fn setup(&mut self, init_addr: u32, mem: &Mem) {
        for b in 0..9 {
            let w0 = mem.r16(init_addr + 6 * b as u32);
            let w1 = mem.r16(init_addr + 6 * b as u32 + 2);
            let delta = mem.r16(init_addr + 6 * b as u32 + 4) as i16 as i32;
            let value = ((w0 as u32) << 16 | w1 as u32) as i32;
            for j in 0..FRAME {
                self.bufs[b][j] = if value == 0 {
                    0
                } else {
                    value.wrapping_add(j as i32 * delta)
                };
            }
        }
    }

    fn download_and_mix(
        &mut self,
        addr: u32,
        vol_main: u16,
        vol_auxa: u16,
        vol_auxb: u16,
        mem: &Mem,
    ) {
        let mut a = addr;
        for (group, vol) in [vol_main, vol_auxa, vol_auxb].into_iter().enumerate() {
            for ch in 0..3 {
                for k in 0..FRAME {
                    let s = mem.r32(a) as i32 as i64;
                    a += 4;
                    self.bufs[group * 3 + ch][k] =
                        self.bufs[group * 3 + ch][k].wrapping_add(((s * vol as i64) >> 15) as i32);
                }
            }
        }
    }

    fn read_pb(mem: &Mem, addr: u32) -> [u16; PB_WORDS] {
        let mut pb = [0u16; PB_WORDS];
        for (i, w) in pb.iter_mut().enumerate() {
            *w = mem.r16(addr + 2 * i as u32);
        }
        pb
    }

    fn write_pb(mem: &mut Mem, addr: u32, pb: &[u16; PB_WORDS]) {
        for (i, w) in pb.iter().enumerate() {
            mem.w16(addr + 2 * i as u32, *w);
        }
    }

    fn process_pb_list(&mut self, mut pb_addr: u32, mem: &mut Mem) {
        let mut guard = 0;
        while pb_addr != 0 && guard < 256 {
            guard += 1;
            let mut pb = Self::read_pb(mem, pb_addr);
            // per-millisecond parameter updates
            let mut updates = [(0u16, 0u16); 32];
            let ua = hilo(pb[39], pb[40]);
            for (i, u) in updates.iter_mut().enumerate() {
                *u = (mem.r16(ua + 4 * i as u32), mem.r16(ua + 4 * i as u32 + 2));
            }
            for ms in 0..5 {
                let start: usize = (0..ms).map(|i| pb[34 + i] as usize).sum();
                if start < updates.len() {
                    let count = pb[34 + ms] as usize;
                    if count <= updates.len() - start {
                        for u in &updates[start..start + count] {
                            if (u.0 as usize) < PB_WORDS {
                                pb[u.0 as usize] = u.1;
                            }
                        }
                    }
                }
                self.process_voice(&mut pb, ms, mem);
            }
            Self::write_pb(mem, pb_addr, &pb);
            pb_addr = hilo(pb[0], pb[1]);
        }
    }

    /// One millisecond (32 samples) of one voice.
    fn process_voice(&mut self, pb: &mut [u16; PB_WORDS], ms: usize, mem: &Mem) {
        const COUNT: usize = 32;
        if pb[7] != 1 {
            return;
        }
        self.voices_run += 1;
        if self.trace && ms == 0 && self.frames % self.trace_every == 0 {
            eprintln!(
                "ax: frame {} voice fmt {:#x} src {} ratio {:#x} mix {:#x} vol {:#x}/{:#x} env {:#x} cur {:#x} end {:#x} loop {} stream {} upd {:?}@{:#x}",
                self.frames, pb[56], pb[4], hilo(pb[83], pb[84]), pb[6], pb[9], pb[11], pb[50], hilo(pb[61], pb[62]), hilo(pb[59], pb[60]), pb[55], pb[8], &pb[34..39], hilo(pb[39], pb[40])
            );
        }
        let mut samples = [0i16; COUNT];
        let mut acc = Accel {
            start: hilo(pb[57], pb[58]) & 0x3FFF_FFFF,
            end: hilo(pb[59], pb[60]) & 0x3FFF_FFFF,
            cur: hilo(pb[61], pb[62]) & 0xBFFF_FFFF,
            format: pb[56],
            gain: pb[79] as i16,
            yn1: pb[81] as i16,
            yn2: pb[82] as i16,
            pred_scale: pb[80] & 0x7F,
            reads_stopped: false,
        };
        let ratio = hilo(pb[83], pb[84]);
        let mut last = [pb[86] as i16, pb[87] as i16, pb[88] as i16, pb[89] as i16];
        let frac = pb[85] as u32;
        let src_type = pb[4];
        let mut pb_tmp = *pb;
        let pos = resample(
            || acc.read_sample(&mut pb_tmp, mem),
            &mut samples,
            COUNT,
            &mut last,
            frac,
            ratio,
            src_type,
        );
        *pb = pb_tmp;
        pb[85] = (pos & 0xFFFF) as u16;
        pb[86..90].copy_from_slice(&[
            last[0] as u16,
            last[1] as u16,
            last[2] as u16,
            last[3] as u16,
        ]);
        pb[61] = (acc.cur >> 16) as u16;
        pb[62] = acc.cur as u16;
        pb[81] = acc.yn1 as u16;
        pb[82] = acc.yn2 as u16;
        pb[80] = acc.pred_scale;

        // volume envelope
        for s in samples.iter_mut() {
            let v = ((*s as i32 * pb[50] as i16 as i32) >> 15) as i64;
            *s = clamp16(v);
            pb[50] = pb[50].wrapping_add(pb[51]);
        }
        if pb[93] != 0 {
            // low-pass filter
            let (a0, b0) = (pb[95] as i32, pb[96] as i16 as i32);
            let mut yn1 = pb[94] as i16;
            for s in samples.iter_mut() {
                let v = clamp16(((a0 * *s as i32 + b0 * yn1 as i32) >> 15) as i64);
                yn1 = v;
                *s = v;
            }
            pb[94] = yn1 as u16;
        }

        let m = convert_mixer_control(pb[6] as u32);
        let on = |b: u32| m & b != 0;
        // (buffer, mixer volume offset, dpop offset, mix bit, ramp bit)
        let routes = [
            (0usize, 9usize, 41usize, MIX_MAIN_L, MIX_MAIN_L_RAMP),
            (1, 11, 44, MIX_MAIN_R, MIX_MAIN_R_RAMP),
            (2, 23, 47, MIX_MAIN_S, MIX_MAIN_S_RAMP),
            (3, 13, 42, MIX_AUXA_L, MIX_AUXA_L_RAMP),
            (4, 15, 45, MIX_AUXA_R, MIX_AUXA_R_RAMP),
            (5, 25, 48, MIX_AUXA_S, MIX_AUXA_S_RAMP),
            (6, 17, 43, MIX_AUXB_L, MIX_AUXB_L_RAMP),
            (7, 19, 46, MIX_AUXB_R, MIX_AUXB_R_RAMP),
            (8, 21, 49, MIX_AUXB_S, MIX_AUXB_S_RAMP),
        ];
        for (buf, vol, dpop, bit, ramp) in routes {
            if on(bit) {
                mix_add(
                    &mut self.bufs[buf][ms * COUNT..(ms + 1) * COUNT],
                    &samples,
                    COUNT,
                    pb,
                    vol,
                    dpop,
                    on(ramp),
                );
            }
        }
    }

    fn mix_aux(&mut self, aux_id: usize, write_addr: u32, read_addr: u32, mem: &mut Mem) {
        let base = if aux_id == 0 { AUXA_L } else { AUXB_L };
        if write_addr != 0 {
            let mut a = write_addr;
            for ch in 0..3 {
                for k in 0..FRAME {
                    mem.w32(a, self.bufs[base + ch][k] as u32);
                    a += 4;
                }
            }
        }
        let mut a = read_addr;
        for ch in 0..3 {
            for k in 0..FRAME {
                self.bufs[MAIN_L + ch][k] =
                    self.bufs[MAIN_L + ch][k].wrapping_add(mem.r32(a) as i32);
                a += 4;
            }
        }
    }

    fn upload_lrs(&mut self, mut dst: u32, mem: &mut Mem) {
        for ch in 0..3 {
            for k in 0..FRAME {
                mem.w32(dst, self.bufs[MAIN_L + ch][k] as u32);
                dst += 4;
            }
        }
    }

    fn set_main_lr(&mut self, src: u32, mem: &Mem) {
        for k in 0..FRAME {
            let s = mem.r32(src + 4 * k as u32) as i32;
            self.bufs[MAIN_L][k] = s;
            self.bufs[MAIN_R][k] = s;
            self.bufs[MAIN_S][k] = 0;
        }
    }

    fn set_opposite_lr(&mut self, src: u32, mem: &Mem) {
        for k in 0..FRAME {
            let s = mem.r32(src + 4 * k as u32) as i32;
            self.bufs[MAIN_L][k] = s.wrapping_neg();
            self.bufs[MAIN_R][k] = s;
            self.bufs[MAIN_S][k] = 0;
        }
    }

    fn compressor(&mut self, threshold: u16, release_frames: u16, table: u32, mem: &Mem) {
        const MILLIS: usize = 5;
        let n = 32 * MILLIS;
        let triggered = (0..n).any(|i| {
            self.bufs[MAIN_L][i].unsigned_abs() > threshold as u32
                || self.bufs[MAIN_R][i].unsigned_abs() > threshold as u32
        });
        let frame_bytes = (32 * MILLIS * 2) as u32;
        let offset;
        if triggered {
            offset = self.compressor_pos as u32 * frame_bytes;
            self.compressor_pos = release_frames;
        } else if self.compressor_pos != 0 {
            self.compressor_pos -= 1;
            offset = (11 + self.compressor_pos as u32) * frame_bytes;
        } else {
            return;
        }
        for i in 0..n {
            let coef = mem.r16(table + offset + 2 * i as u32) as i64;
            self.bufs[MAIN_L][i] = ((self.bufs[MAIN_L][i] as i64 * coef) >> 15) as i32;
            self.bufs[MAIN_R][i] = ((self.bufs[MAIN_R][i] as i64 * coef) >> 15) as i32;
        }
    }

    fn output(&mut self, lr: u32, surround: u32, mem: &mut Mem) {
        for k in 0..FRAME {
            mem.w32(surround + 4 * k as u32, self.bufs[MAIN_S][k] as u32);
        }
        for k in 0..FRAME {
            let l = clamp16(self.bufs[MAIN_L][k] as i64);
            let r = clamp16(self.bufs[MAIN_R][k] as i64);
            // interleaved R, L as the ucode writes them (the AI plays word 1 as the left channel)
            mem.w16(lr + 4 * k as u32, r as u16);
            mem.w16(lr + 4 * k as u32 + 2, l as u16);
        }
    }

    fn mix_auxb_lr(&mut self, ul: u32, dl: u32, mem: &mut Mem) {
        for k in 0..FRAME {
            mem.w32(ul + 4 * k as u32, self.bufs[AUXB_L][k] as u32);
            mem.w32(ul + 4 * (FRAME + k) as u32, self.bufs[AUXB_L + 1][k] as u32);
        }
        for k in 0..FRAME {
            let s = mem.r32(dl + 4 * k as u32) as i32;
            self.bufs[AUXB_L][k] = s;
            self.bufs[MAIN_L][k] = self.bufs[MAIN_L][k].wrapping_add(s);
        }
        for k in 0..FRAME {
            let s = mem.r32(dl + 4 * (FRAME + k) as u32) as i32;
            self.bufs[AUXB_L + 1][k] = s;
            self.bufs[MAIN_R][k] = self.bufs[MAIN_R][k].wrapping_sub(s);
        }
    }

    fn send_aux_and_mix(&mut self, a: [u32; 6], mem: &mut Mem) {
        let [auxa_up, auxb_s_up, main_l_dl, main_r_dl, auxb_l_dl, auxb_r_dl] = a;
        for ch in 0..3 {
            for k in 0..FRAME {
                mem.w32(
                    auxa_up + 4 * (ch * FRAME + k) as u32,
                    self.bufs[AUXA_L + ch][k] as u32,
                );
            }
        }
        for k in 0..FRAME {
            mem.w32(auxb_s_up + 4 * k as u32, self.bufs[AUXB_L + 2][k] as u32);
        }
        for (dst, addr) in [
            (MAIN_L, main_l_dl),
            (MAIN_R, main_r_dl),
            (AUXB_L, auxb_l_dl),
            (AUXB_L + 1, auxb_r_dl),
        ] {
            for k in 0..FRAME {
                self.bufs[dst][k] =
                    self.bufs[dst][k].wrapping_add(mem.r32(addr + 4 * k as u32) as i32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: u32 = 0x1000;
    const PB: u32 = 0x2000;
    const OUT: u32 = 0x4000;
    const SURR: u32 = 0x6000;

    fn put16(ram: &mut [u8], addr: u32, v: u16) {
        ram[addr as usize..addr as usize + 2].copy_from_slice(&v.to_be_bytes());
    }

    /// Command list: process the PB list at PB, output to OUT, end.
    fn write_cmdlist(ram: &mut [u8]) -> usize {
        let words: [u16; 12] = [
            0x02,
            0,
            PB as u16,
            0x03,
            0x0E,
            0,
            SURR as u16,
            0,
            OUT as u16,
            0x0F,
            0,
            0,
        ];
        for (i, w) in words.iter().enumerate() {
            put16(ram, CMD + 2 * i as u32, *w);
        }
        10
    }

    /// A running voice with a unity main L/R mix, no ramps.
    fn write_pb(ram: &mut [u8], format: u16, src_type: u16, cur: u32, end: u32) {
        let mut w = [0u16; PB_WORDS];
        w[4] = src_type;
        w[6] = 0x0003; // main L + R
        w[7] = 1;
        w[9] = 0x7FFF; // main left volume
        w[11] = 0x7FFF; // main right volume
        w[50] = 0x7FFF; // volume envelope
        w[56] = format;
        w[59] = (end >> 16) as u16;
        w[60] = end as u16;
        w[61] = (cur >> 16) as u16;
        w[62] = cur as u16;
        w[79] = 0x800; // gain
        w[83] = 1; // ratio 1.0 (hi), lo = 0
        for (i, v) in w.iter().enumerate() {
            put16(ram, PB + 2 * i as u32, *v);
        }
    }

    fn render(ram: &mut [u8], aram: &[u8]) -> Ax {
        let size = write_cmdlist(ram);
        let mut ax = Ax::new();
        let mut mem = Mem { ram, aram };
        assert_eq!(ax.handle_mail(MAIL_CMDLIST | size as u32, &mut mem), None);
        assert_eq!(ax.handle_mail(CMD, &mut mem), Some(DSP_YIELD));
        ax
    }

    fn out_frame(ram: &[u8], k: usize) -> (i16, i16) {
        let a = OUT as usize + 4 * k;
        (
            i16::from_be_bytes([ram[a], ram[a + 1]]),
            i16::from_be_bytes([ram[a + 2], ram[a + 3]]),
        )
    }

    #[test]
    fn pcm16_voice_reaches_the_output_in_the_ucodes_right_left_order() {
        let mut ram = vec![0u8; 0x8000];
        let mut aram = vec![0u8; 0x1000];
        for i in 0..300u32 {
            let v = (i as i16 - 150) * 100;
            aram[2 * i as usize..2 * i as usize + 2].copy_from_slice(&v.to_be_bytes());
        }
        write_pb(&mut ram, 0x000A, 2, 0, 0x7FF);
        render(&mut ram, &aram);
        for k in [0usize, 1, 50, 159] {
            let expected = (k as i32 - 150) * 100;
            let (right, left) = out_frame(&ram, k);
            // three 15-bit gain stages (envelope, mixer) lose at most a couple of LSBs
            assert!(
                (right as i32 - expected).abs() <= 3,
                "right {right} vs {expected} at {k}"
            );
            assert_eq!(left, right);
        }
    }

    #[test]
    fn adpcm_frame_decodes_nibbles_scaled_by_the_header() {
        let mut ram = vec![0u8; 0x8000];
        let mut aram = vec![0u8; 0x1000];
        // header: predictor 0, scale 1 (x2); then nibbles 1,2,3,4,5,6,7,8(-8),...
        aram[0] = 0x01;
        let nibbles: Vec<u8> = (1..=14).map(|n| n as u8 & 0xF).collect();
        for (i, pair) in nibbles.chunks(2).enumerate() {
            aram[1 + i] = pair[0] << 4 | pair[1];
        }
        // ADPCM address is in nibbles: skip the one-byte header (2 nibbles)
        write_pb(&mut ram, 0x0000, 2, 2, 0x1F);
        // coefficients stay 0, initial pred_scale comes from the PB (scale 1)
        put16(&mut ram, PB + 2 * 80, 0x0001);
        render(&mut ram, &aram);
        let want = [2, 4, 6, 8, 10, 12, 14, -16, -14, -12, -10, -8];
        for (k, w) in want.iter().enumerate() {
            let (r, _) = out_frame(&ram, k);
            // two 15-bit gain stages with a 0x7FFF "unity" floor each lose one LSB
            assert!((r as i32 - *w).abs() <= 2, "sample {k}: {r} vs {w}");
        }
    }

    #[test]
    fn one_shot_voice_stops_at_its_end() {
        let mut ram = vec![0u8; 0x8000];
        let aram = vec![0u8; 0x1000];
        write_pb(&mut ram, 0x000A, 2, 0, 10);
        render(&mut ram, &aram);
        let running = u16::from_be_bytes([ram[(PB + 14) as usize], ram[(PB + 15) as usize]]);
        assert_eq!(running, 0);
    }
}
