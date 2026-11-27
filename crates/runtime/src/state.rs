//! Save states and rewind (ADR 0035).
//!
//! A state is a byte string: guest RAM, the machine's registers and timers, the hardware (VI, SI, DI, AI,
//! DSP with ARAM, EXI with the memory cards, the GX pipeline, the video backend's framebuffer and EFB
//! copies) and the guest threads. Threads carry no host stack: each is its `Context` plus the place it is
//! parked at, and a loaded thread gets its host frames rebuilt from its guest stack (`hle::resume_thread`).
//!
//! States are taken and loaded by the guest at a poll point (`fepor_poll`, the safe point where interrupts
//! are delivered too), where every register of the running thread is in its `Context`.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use fepor_core::settings::SettingsHandle;
use fepor_core::snap::{R, W};
use fepor_core::states::{self, Command};

use crate::context::{Context, Fpr};
use crate::machine::{machine, RAM_SIZE};
use crate::sched::{self, Image, IrqFrame, ParkKind, ThreadImage};

/// Layout of the state; bump it when a component changes what it writes.
const VERSION: u32 = 1;
/// Seconds between the states of the rewind buffer.
const REWIND_INTERVAL: Duration = Duration::from_secs(1);
/// Time between two steps back while the rewind key is held.
const REWIND_STEP: Duration = Duration::from_millis(120);

// ---- time ----------------------------------------------------------------------------------------------

// Times in a state are guest-clock values (`crate::clock`, ADR 0036): offsets from the guest's "now", so they
// do not depend on the speed the state was taken or is loaded at.

/// Guest microseconds from now until the guest-clock time `t_ns` (0 when it is past).
pub fn until(t_ns: u64) -> u64 {
    t_ns.saturating_sub(crate::clock::now_ns()) / 1000
}

/// The guest-clock time `micros` guest microseconds from now.
pub fn from_now(micros: u64) -> u64 {
    crate::clock::now_ns() + micros * 1000
}

/// The guest-clock time `micros` guest microseconds ago.
pub fn ago(micros: u64) -> u64 {
    crate::clock::now_ns().saturating_sub(micros * 1000)
}

// ---- capture and apply -----------------------------------------------------------------------------------

fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut chunks = b.chunks_exact(8);
    for c in &mut chunks {
        h = (h ^ u64::from_le_bytes(c.try_into().unwrap())).wrapping_mul(0x100_0000_01b3);
        h ^= h >> 29;
    }
    for &x in chunks.remainder() {
        h = (h ^ x as u64).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn write_frame(w: &mut W, f: &IrqFrame) {
    for p in &f.f {
        w.f64(p.ps0);
        w.f64(p.ps1);
    }
    w.u32(f.fpscr);
    w.u32(f.pc);
}

fn read_frame(r: &mut R) -> IrqFrame {
    let mut f = [Fpr { ps0: 0.0, ps1: 0.0 }; 32];
    for p in &mut f {
        p.ps0 = r.f64();
        p.ps1 = r.f64();
    }
    IrqFrame {
        f,
        fpscr: r.u32(),
        pc: r.u32(),
    }
}

fn write_threads(w: &mut W, img: &Image) {
    w.u32(img.threads.len() as u32);
    for t in &img.threads {
        w.bytes(&t.ctx);
        w.opt_u32(t.last_save);
        w.opt_u32(t.irq_ctx);
        w.bool(t.irq_frame.is_some());
        if let Some(f) = &t.irq_frame {
            write_frame(w, f);
        }
        w.bool(t.park.is_some());
        let (ctx, kind) = t.park.unwrap_or((0, ParkKind::AtSave));
        w.u32(ctx);
        w.u8((kind == ParkKind::AfterIrq) as u8);
    }
}

fn read_threads(r: &mut R) -> Option<Image> {
    let n = r.u32() as usize;
    if n == 0 || n > 4096 {
        return None;
    }
    let mut threads = Vec::new();
    for _ in 0..n {
        let ctx = r.bytes().to_vec();
        if ctx.len() != std::mem::size_of::<Context>() {
            return None;
        }
        let last_save = r.opt_u32();
        let irq_ctx = r.opt_u32();
        let irq_frame = r.bool().then(|| read_frame(r));
        let parked = r.bool();
        let (pctx, kind) = (r.u32(), r.u8());
        threads.push(ThreadImage {
            ctx,
            last_save,
            irq_ctx,
            irq_frame,
            park: parked.then_some((
                pctx,
                if kind == 1 {
                    ParkKind::AfterIrq
                } else {
                    ParkKind::AtSave
                },
            )),
        });
    }
    (!r.failed).then_some(Image { threads })
}

/// The disc's identification at the start of main memory: game id, maker, disc number, version.
pub fn disc_id() -> [u8; 8] {
    machine().ram[..8].try_into().unwrap()
}

/// What the running thread's state is, for the trace that proves a load restored it.
fn trace_line(what: &str) {
    let m = machine();
    let ram = hash_bytes(&m.ram[..RAM_SIZE]);
    let efb =
        m.hw.efb_hash()
            .map_or("-".to_string(), |h| format!("{h:016x}"));
    eprintln!(
        "state: {what}: ram {ram:016x} efb {efb} retraces {}",
        m.hw.retraces
    );
}

static TRACE: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("FEPOR_TRACE").is_ok_and(|v| v.split(',').any(|x| x == "state" || x == "all"))
});

fn trace_on() -> bool {
    *TRACE
}

/// Take a state of the machine as it is now. Must run on the guest thread at a poll point; `None` when a
/// thread is inside an interrupt handler in a way a state cannot describe (try again at the next one).
pub fn capture() -> Option<Vec<u8>> {
    let started = Instant::now();
    let img = sched::capture()?;
    let m = machine();
    let mut w = W::new();
    w.buf.reserve(RAM_SIZE + (16 << 20) + (1 << 20));
    w.section(b"FEPS", |w| {
        w.u32(VERSION);
        w.buf.extend_from_slice(&disc_id());
    });
    w.section(b"MACH", |w| {
        w.u64(m.timebase());
        w.u32(m.enter_count);
        for &v in &m.cpu_sprs {
            w.u32(v);
        }
        w.bool(m.dec_deadline.is_some());
        w.u64(m.dec_deadline.unwrap_or(0));
    });
    w.section(b"RAM ", |w| w.bytes(&m.ram[..RAM_SIZE]));
    w.section(b"HW  ", |w| m.hw.save_state(w));
    w.section(b"THRD", |w| write_threads(w, &img));
    if trace_on() {
        trace_line(&format!(
            "captured {:.1} MB in {:.1} ms",
            w.buf.len() as f64 / 1e6,
            started.elapsed().as_secs_f64() * 1e3
        ));
    }
    Some(w.buf)
}

/// Load a state. Must run on the guest thread at a poll point; `c` is the running thread's context. On
/// success every host frame of the guest is abandoned and the loaded threads take over: the caller returns
/// at once (the generated code unwinds because `Context::spr[SPR_UNWIND]` is set). The state is checked
/// before anything is touched; a state that fails later (a component that reads less than was written)
/// ends the process, because the machine is half replaced.
pub fn apply(c: &mut Context, blob: &[u8], source: &str) -> Result<(), String> {
    if !crate::abi::jit_active() {
        return Err("save states need the native code backend".into());
    }
    let mut r = R::new(blob);
    let mut head = r.section(b"FEPS");
    if r.failed || head.u32() != VERSION {
        return Err("this state was made by another version of the program".into());
    }
    if head.array8() != disc_id() {
        return Err("this state belongs to another game or disc version".into());
    }
    let mut mach = r.section(b"MACH");
    let mut ram = r.section(b"RAM ");
    let mut hw = r.section(b"HW  ");
    let mut thrd = r.section(b"THRD");
    if r.failed {
        return Err("the state is damaged".into());
    }
    let Some(img) = read_threads(&mut thrd) else {
        return Err("the state is damaged".into());
    };
    let ram_bytes = ram.bytes();
    if ram_bytes.len() != RAM_SIZE {
        return Err("the state is damaged".into());
    }

    sched::kill_others();
    // the card image goes back with the state: keep the current one as a backup first (ADR 0035)
    states::guard_card();
    let m = machine();
    m.ram[..RAM_SIZE].copy_from_slice(ram_bytes);
    // Patch sets and held cheats belong to the session, not to the state: write the enabled ones over the
    // restored memory again. The originals they saved when enabled stay as they are.
    m.patches.reapply(&mut m.ram);
    let tb = mach.u64();
    // the guest clock continues from the state's time base, at the speed in force now
    crate::clock::set_ns((tb as u128 * 10_000 / 405) as u64);
    m.enter_count = mach.u32();
    for v in &mut m.cpu_sprs {
        *v = mach.u32();
    }
    let has_dec = mach.bool();
    let dec = mach.u64();
    m.dec_deadline = has_dec.then_some(dec);
    let ram_ptr = m.ram_ptr();
    // SAFETY: the guest is single-threaded at this point; the hardware only reads RAM during the load.
    let ram_ref = unsafe { std::slice::from_raw_parts(ram_ptr, RAM_SIZE) };
    m.hw.load_state(&mut hw, ram_ref);
    if mach.failed || hw.failed || !hw.at_end() {
        crate::abi::die_str("a save state did not match this build's layout while loading");
    }
    sched::install(&img, ram_ptr);
    c.spr[crate::abi::SPR_UNWIND] = 1;
    fepor_core::bugreport::note(
        "save state",
        format!("a state was loaded in this run: {source}"),
    );
    if trace_on() {
        trace_line("loaded");
    }
    Ok(())
}

// ---- control: slots, rewind --------------------------------------------------------------------------------

struct Delta {
    /// Length of the state this delta turns the newer one back into.
    prev_len: usize,
    /// 4 KiB pages that differ, and their bytes (xor of the two states), deflated.
    pages: Vec<u32>,
    data: Vec<u8>,
}

const PAGE: usize = 4096;

fn make_delta(newer: &[u8], older: &[u8]) -> Delta {
    let len = newer.len().max(older.len());
    let mut pages = Vec::new();
    let mut raw = Vec::new();
    let byte = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    for p in 0..len.div_ceil(PAGE) {
        let (a, b) = (p * PAGE, ((p + 1) * PAGE).min(len));
        let (n, o) = (
            newer.get(a..b.min(newer.len())).unwrap_or(&[]),
            older.get(a..b.min(older.len())).unwrap_or(&[]),
        );
        if n == o {
            continue;
        }
        pages.push(p as u32);
        raw.extend((a..b).map(|i| byte(newer, i) ^ byte(older, i)));
    }
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    let _ = z.write_all(&raw);
    Delta {
        prev_len: older.len(),
        pages,
        data: z.finish().unwrap_or_default(),
    }
}

/// Turn `state` (the newer one) back into the older one the delta was made against.
fn undo_delta(state: &mut Vec<u8>, d: &Delta) {
    let mut raw = Vec::new();
    let _ = flate2::read::DeflateDecoder::new(&d.data[..]).read_to_end(&mut raw);
    let len = state.len().max(d.prev_len);
    state.resize(len, 0);
    let mut at = 0;
    for &p in &d.pages {
        let a = p as usize * PAGE;
        let b = (a + PAGE).min(len);
        for (x, y) in state[a..b].iter_mut().zip(&raw[at..at + (b - a)]) {
            *x ^= y;
        }
        at += b - a;
    }
    state.truncate(d.prev_len);
}

enum Msg {
    Push {
        blob: Vec<u8>,
        max_states: usize,
        max_bytes: usize,
    },
    Pop(mpsc::Sender<Option<Vec<u8>>>),
    Clear,
}

/// The rewind buffer lives on its own thread: making a delta of 40 MB must not stall the guest.
/// It keeps the newest state whole and each older one as the difference to its successor.
fn ring_thread(rx: mpsc::Receiver<Msg>) {
    let mut newest: Option<Vec<u8>> = None;
    let mut deltas: VecDeque<Delta> = VecDeque::new();
    let mut bytes = 0usize;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Push {
                blob,
                max_states,
                max_bytes,
            } => {
                if let Some(prev) = newest.take() {
                    let d = make_delta(&blob, &prev);
                    bytes += d.data.len() + d.pages.len() * 4;
                    deltas.push_back(d);
                }
                newest = Some(blob);
                while deltas.len() + 1 > max_states || (bytes > max_bytes && !deltas.is_empty()) {
                    if let Some(d) = deltas.pop_front() {
                        bytes -= d.data.len() + d.pages.len() * 4;
                    }
                }
            }
            Msg::Pop(reply) => {
                // The newest state is what the first step back loads; each further step moves one older.
                let out = newest.clone();
                if let (Some(n), Some(d)) = (newest.as_mut(), deltas.pop_back()) {
                    bytes -= d.data.len() + d.pages.len() * 4;
                    undo_delta(n, &d);
                }
                let _ = reply.send(out);
            }
            Msg::Clear => {
                newest = None;
                deltas.clear();
                bytes = 0;
            }
        }
        states::set_rewind_seconds(
            (newest.is_some() as usize + deltas.len()) as u32 * REWIND_INTERVAL.as_secs() as u32,
        );
    }
}

/// `FEPOR_STATES=save1@300,load1@600,rewind@900+60`: scripted commands for tests, at retrace counts (a
/// rewind key held from the first count for the second number of retraces, 20 ms each; wall-clock time,
/// because the count itself goes back while rewinding).
enum Step {
    Cmd(Command),
    Rewind(u64),
}

static SCRIPT: LazyLock<Vec<(u64, Step)>> = LazyLock::new(|| {
    let Ok(spec) = std::env::var("FEPOR_STATES") else {
        return Vec::new();
    };
    spec.split(',')
        .filter_map(|item| {
            let (what, when) = item.split_once('@')?;
            let (at, len) = when.split_once('+').unwrap_or((when, "0"));
            let slot = |p: &str| p.parse::<u8>().ok().map(|n| n.saturating_sub(1));
            let step = match what {
                "rewind" => Step::Rewind(len.parse().ok()?),
                w if w.starts_with("save") => Step::Cmd(Command::Save(slot(&w[4..])?)),
                w if w.starts_with("load") => Step::Cmd(Command::Load(slot(&w[4..])?)),
                _ => return None,
            };
            Some((at.parse().ok()?, step))
        })
        .collect()
});

struct Control {
    hold_until: Option<Instant>,
    script_done: Vec<bool>,
    settings: SettingsHandle,
    settings_gen: u64,
    rewind: bool,
    rewind_seconds: u32,
    ring: mpsc::Sender<Msg>,
    next_capture: Instant,
    next_step: Instant,
    /// A Save command that found no safe moment yet.
    retries: u32,
    was_holding: bool,
}

static CONTROL: Mutex<Option<Control>> = Mutex::new(None);
/// A state read from a file, waiting for the guest's next poll point.
static LOADED: Mutex<Option<(Vec<u8>, String)>> = Mutex::new(None);
static LOADED_READY: AtomicBool = AtomicBool::new(false);
/// Cheap "anything to do?" for the poll fast path.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set up the states of this run: the folder is `<data>/states/<game id>-v<version>` and the settings say
/// whether a rewind buffer is kept. Call after the boot data is in RAM (the disc id is read from there).
pub fn configure(settings: SettingsHandle, data: &std::path::Path) {
    let id = disc_id();
    let dir = data
        .join("states")
        .join(format!("{}-v{}", String::from_utf8_lossy(&id[..6]), id[7]));
    states::set_dir(dir);
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("rewind".into())
        .spawn(move || ring_thread(rx));
    *CONTROL.lock().unwrap_or_else(|e| e.into_inner()) = Some(Control {
        settings,
        hold_until: None,
        script_done: vec![false; SCRIPT.len()],
        settings_gen: u64::MAX,
        rewind: false,
        rewind_seconds: 60,
        ring: tx,
        next_capture: Instant::now() + REWIND_INTERVAL,
        next_step: Instant::now(),
        retries: 0,
        was_holding: false,
    });
    ACTIVE.store(true, Ordering::Release);
    fepor_core::bugreport::note("save state", "no state was loaded in this run");
}

/// The guest's safe point: run what the front ends asked for.
pub fn poll(c: &mut Context) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let mut guard = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(ctl) = guard.as_mut() else { return };
    let gen = ctl.settings.generation();
    if gen != ctl.settings_gen {
        ctl.settings_gen = gen;
        let s = ctl.settings.get().states;
        if ctl.rewind && !s.rewind {
            let _ = ctl.ring.send(Msg::Clear);
        }
        (ctl.rewind, ctl.rewind_seconds) = (s.rewind, s.rewind_seconds);
    }
    if !SCRIPT.is_empty() {
        let now = machine().hw.retraces;
        for (i, (at, step)) in SCRIPT.iter().enumerate() {
            if now < *at || ctl.script_done[i] {
                continue;
            }
            ctl.script_done[i] = true;
            match step {
                Step::Cmd(cmd) => states::post(*cmd),
                Step::Rewind(len) => {
                    ctl.hold_until = Some(Instant::now() + Duration::from_millis(20 * len))
                }
            }
        }
        if ctl.hold_until.is_some() {
            let held = ctl.hold_until.is_some_and(|t| Instant::now() < t);
            states::set_rewind_held(held);
            if !held {
                ctl.hold_until = None;
            }
        }
    }
    let holding = ctl.rewind && states::rewind_held();
    if !(states::pending() || LOADED_READY.load(Ordering::Acquire) || ctl.rewind || holding) {
        return;
    }

    if let Some(cmd) = states::peek() {
        if !crate::abi::jit_active() {
            // the generated C++ has no resume entries (docs/adr/0035)
            states::take();
            states::set_message("Save states need the native code backend");
            return;
        }
        match cmd {
            Command::Save(n) => match capture() {
                Some(blob) => {
                    states::take();
                    ctl.retries = 0;
                    save_to_slot(n, blob);
                }
                None => {
                    ctl.retries += 1;
                    if ctl.retries > 20_000 {
                        states::take();
                        ctl.retries = 0;
                        states::set_message(
                            "No safe moment to save (an interrupt handler is running)",
                        );
                    }
                }
            },
            Command::Load(n) => {
                states::take();
                load_from_slot(n);
            }
        }
        return;
    }

    if LOADED_READY.swap(false, Ordering::AcqRel) {
        let loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some((blob, source)) = loaded {
            match apply(c, &blob, &source) {
                Ok(()) => {
                    ctl.next_capture = Instant::now() + REWIND_INTERVAL;
                    states::set_message(format!("Loaded slot {}", states::slot() + 1));
                }
                Err(e) => states::set_message(format!("Cannot load: {e}")),
            }
        }
        return;
    }

    if holding {
        ctl.was_holding = true;
        if Instant::now() < ctl.next_step {
            return;
        }
        ctl.next_step = Instant::now() + REWIND_STEP;
        let (tx, rx) = mpsc::channel();
        let _ = ctl.ring.send(Msg::Pop(tx));
        let Ok(Some(blob)) = rx.recv() else {
            return;
        };
        if let Err(e) = apply(c, &blob, "stepped back in the rewind buffer") {
            states::set_message(format!("Cannot rewind: {e}"));
        }
        return;
    }
    if ctl.was_holding {
        ctl.was_holding = false;
        ctl.next_capture = Instant::now() + REWIND_INTERVAL;
    }
    if ctl.rewind && Instant::now() >= ctl.next_capture {
        if let Some(blob) = capture() {
            ctl.next_capture = Instant::now() + REWIND_INTERVAL;
            let _ = ctl.ring.send(Msg::Push {
                blob,
                max_states: ctl.rewind_seconds as usize,
                max_bytes: (states::REWIND_MEMORY_MIB as usize) << 20,
            });
        }
    }
}

fn save_to_slot(n: u8, blob: Vec<u8>) {
    let Some(path) = states::slot_path(n) else {
        states::set_message("Save states are not set up");
        return;
    };
    let id = disc_id();
    let _ = std::thread::Builder::new()
        .name("state-write".into())
        .spawn(move || match states::write_file(&path, &id, &blob) {
            Ok(()) => states::set_message(format!("Saved slot {}", n + 1)),
            Err(e) => states::set_message(format!("Cannot save slot {}: {e}", n + 1)),
        });
}

fn load_from_slot(n: u8) {
    let Some(path) = states::slot_path(n) else {
        states::set_message("Save states are not set up");
        return;
    };
    let id = disc_id();
    let _ = std::thread::Builder::new()
        .name("state-read".into())
        .spawn(move || {
            let age = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map_or("unknown age".to_string(), |d| {
                    format!("saved {} s before", d.as_secs())
                });
            match states::read_file(&path, &id) {
                Ok(blob) => {
                    let source = format!("slot {}, {age}", n + 1);
                    *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = Some((blob, source));
                    LOADED_READY.store(true, Ordering::Release);
                }
                Err(e) => states::set_message(format!("Cannot load slot {}: {e}", n + 1)),
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_walk_back_through_changes_and_sizes() {
        let a: Vec<u8> = (0..20_000u32).map(|i| (i * 7) as u8).collect();
        let mut b = a.clone();
        b[5] ^= 0xFF;
        b[9000] = 1;
        b.extend_from_slice(&[1, 2, 3]);
        let mut c = b.clone();
        c.truncate(10_000);
        c[4097] = 9;
        let (dab, dbc) = (make_delta(&b, &a), make_delta(&c, &b));
        let mut s = c.clone();
        undo_delta(&mut s, &dbc);
        assert_eq!(s, b);
        undo_delta(&mut s, &dab);
        assert_eq!(s, a);
    }
}
