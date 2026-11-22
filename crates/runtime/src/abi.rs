//! The C ABI the generated code links against (declared in `runtime/include/fepor/ppc.h`).

use crate::context::Context;
use crate::machine::machine;

pub type HostFn = unsafe extern "C" fn(*mut Context);

static CURRENT: std::sync::atomic::AtomicPtr<Context> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Register the context of the running guest thread (used for crash reports).
pub fn set_current(c: *mut Context) {
    CURRENT.store(c, std::sync::atomic::Ordering::Relaxed);
}

/// `FEPOR_WATCHDOG=<seconds>`: after that long, print where the guest is (recent calls, registers) and
/// exit with status 124. Meant for finding spin loops while bringing the boot up.
pub fn start_watchdog() {
    let Some(secs) = std::env::var("FEPOR_WATCHDOG")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return;
    };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(secs));
        eprintln!("fepor: watchdog: guest still running after {secs}s");
        // The guest thread keeps running while we read its state: good enough for a crash report.
        let c = CURRENT.load(std::sync::atomic::Ordering::Relaxed);
        if !c.is_null() {
            // SAFETY: set_current stores a context that outlives the guest run.
            let c = unsafe { &*c };
            eprintln!(
                "  r1={:#010x} lr={:#010x} ctr={:#010x} r3={:#010x} msr={:#06x}",
                c.r[1], c.lr, c.ctr, c.r[3], c.msr
            );
        }
        let recent = machine()
            .trace
            .recent(if std::env::var_os("FEPOR_RECENT").is_some() {
                4096
            } else {
                32
            });
        eprintln!(
            "  recent calls (oldest first): {}",
            recent
                .iter()
                .map(|a| format!("{a:08x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let fs = &machine().trace.first_seen;
        // `FEPOR_FIRST_SEEN=<file>`: every function in the order it first ran (compare two backends or runs)
        if let Some(path) = std::env::var_os("FEPOR_FIRST_SEEN") {
            let text: String = fs.iter().map(|a| format!("{a:08x}\n")).collect();
            let _ = std::fs::write(path, text);
        }
        if !fs.is_empty() {
            let from = machine()
                .trace
                .mark
                .unwrap_or(fs.len().saturating_sub(120))
                .max(fs.len().saturating_sub(120));
            let tail = &fs[from..];
            eprintln!(
                "  newest functions (first execution order): {}",
                tail.iter()
                    .map(|a| format!("{a:08x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        let am = &machine().trace.after_mark;
        if !am.is_empty() {
            eprintln!(
                "  calls after mark ({}): {}",
                am.len(),
                am.iter()
                    .map(|a| format!("{a:08x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        // OS threads: __OSActiveThreadQueue head at 0x800000DC, linkActive at +0x2FC.
        let m = machine();
        if !c.is_null() {
            // SAFETY: as above.
            let r13 = unsafe { &*c }.r[13];
            eprintln!(
                "  r13={r13:#010x} [r13-0x65a8]={:#010x} (draw-done flag in the high byte)",
                m.read_u32(r13.wrapping_sub(0x65a8))
            );
        }
        let mut t = m.read_u32(0x8000_00DC);
        for _ in 0..32 {
            if t == 0 {
                break;
            }
            let st = m.read_u32(t + 0x2C8) >> 16;
            eprintln!(
                "  thread {t:08x} state={st} prio={} srr0={:08x} lr={:08x} wait-queue={:08x}",
                m.read_u32(t + 0x2D0) as i32,
                m.read_u32(t + 0x198),
                m.read_u32(t + 0x84),
                m.read_u32(t + 0x2DC)
            );
            let mut sp = m.read_u32(t + 4);
            let mut bt = String::new();
            for _ in 0..8 {
                if !(0x8000_0000..0x8180_0000).contains(&sp) {
                    break;
                }
                let back = m.read_u32(sp);
                bt += &format!(" {:08x}", m.read_u32(back.wrapping_add(4)));
                sp = back;
            }
            eprintln!("    backtrace:{bt}");
            t = m.read_u32(t + 0x2FC);
        }
        std::process::exit(124);
    });
}

/// Report a fatal guest error with state and recent calls, then exit with status 70.
pub fn die_str(msg: &str) -> ! {
    eprintln!("fepor: fatal: {msg}");
    crate::hle::dump_log();
    let c = CURRENT.load(std::sync::atomic::Ordering::Relaxed);
    if !c.is_null() {
        // SAFETY: set_current stores a context that outlives the guest run.
        let c = unsafe { &*c };
        eprintln!(
            "  r1={:#010x} lr={:#010x} ctr={:#010x}",
            c.r[1], c.lr, c.ctr
        );
        eprintln!(
            "  r3={:#010x} r4={:#010x} r5={:#010x} r6={:#010x}",
            c.r[3], c.r[4], c.r[5], c.r[6]
        );
    }
    let recent = machine().trace.recent(32);
    if !recent.is_empty() {
        eprintln!(
            "  recent calls (oldest first): {}",
            recent
                .iter()
                .map(|a| format!("{a:08x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    std::process::exit(70);
}

#[no_mangle]
pub extern "C" fn fepor_unimplemented(_c: *mut Context, addr: u32, code: u32) -> ! {
    die_str(&format!(
        "unimplemented instruction {code:08x} at {addr:08x}"
    ))
}

#[no_mangle]
pub extern "C" fn fepor_unresolved(_c: *mut Context, from: u32, target: u32) -> ! {
    die_str(&format!(
        "control flow from {from:08x} to {target:08x} has no generated function"
    ))
}

#[no_mangle]
pub extern "C" fn fepor_trap(_c: *mut Context, addr: u32) -> ! {
    die_str(&format!("trap at {addr:08x}"))
}

#[no_mangle]
pub extern "C" fn fepor_syscall(_c: *mut Context, addr: u32) {
    // The only guest user is the SDK's instruction-cache flush handler: nothing to do for recompiled code.
    if machine().trace.hw {
        eprintln!("sc at {addr:08x} (ignored)");
    }
}

/// `rfi` outside exception handling is how the SDK switches address mode: it jumps to SRR0 with a new MSR.
/// SRR0 equal to LR means "return to the caller" (the generated function simply returns afterwards);
/// anything else is a jump to a function, executed as a call (the callee returns to our caller).
#[no_mangle]
pub extern "C" fn fepor_rfi(c: *mut Context, addr: u32) {
    // SAFETY: the generated code passes its live context.
    let c = unsafe { &mut *c };
    let srr0 = c.spr[SRR0];
    if srr0 == c.lr {
        return;
    }
    let target = 0x8000_0000 | (srr0 & 0x3FFF_FFFF); // real-mode addresses are physical
    match guest_function(target) {
        // SAFETY: generated functions take the guest context.
        Some(f) => unsafe { f(c) },
        None => die_str(&format!(
            "rfi at {addr:08x} to {srr0:#010x}: no generated function"
        )),
    }
}

const SRR0: usize = 26;

/// Where guest functions come from when they are not the generated static library: the native code
/// backend (`crates/jit`) registers itself here.
static RESOLVER: std::sync::OnceLock<fn(u32) -> Option<HostFn>> = std::sync::OnceLock::new();

pub fn set_resolver(f: fn(u32) -> Option<HostFn>) {
    let _ = RESOLVER.set(f);
}

/// True when guest code is compiled in process: a thread resumed after an interrupt-time context switch
/// then abandons its host frames by returning (`Context::spr[1022]`), not with a C++ exception.
pub fn jit_active() -> bool {
    RESOLVER.get().is_some()
}

/// `Context::spr` slot that is non-zero while the JIT's frames are being abandoned.
pub const SPR_UNWIND: usize = 1022;

pub fn guest_function(target: u32) -> Option<HostFn> {
    if let Some(r) = RESOLVER.get() {
        return r(target);
    }
    #[cfg(feature = "dispatch")]
    return dispatch::find(target);
    #[cfg(not(feature = "dispatch"))]
    None
}

#[no_mangle]
pub extern "C" fn fepor_mftb(tbr: u32) -> u32 {
    let tb = machine().timebase();
    if tbr == 269 {
        (tb >> 32) as u32
    } else {
        tb as u32
    }
}

/// `mtspr DMA_L`: with the trigger bit set, run the locked-cache DMA described by DMA_U/DMA_L and clear the bit.
#[no_mangle]
pub extern "C" fn fepor_lc_dma(c: *mut Context) {
    // SAFETY: the generated code passes its live context.
    let c = unsafe { &mut *c };
    let (u, l) = (c.spr[922], c.spr[923]);
    if l & 2 == 0 {
        return;
    }
    let blocks = match ((u & 0x1F) << 2) | ((l >> 2) & 3) {
        0 => 128,
        n => n as usize,
    };
    let m = machine();
    m.hw.lc_dma(u & !0x1F, l & !0x1F, blocks, l & 0x10 != 0, &mut m.ram);
    c.spr[923] &= !3;
}

/// `mtdec`: the decrementer raises exception 8 when it passes zero, `value + 1` ticks from now.
#[no_mangle]
pub extern "C" fn fepor_mtdec(value: u32) {
    let m = machine();
    m.dec_deadline = Some(m.timebase() + value as u64 + 1);
}

/// `mfdec`: ticks left until expiry (wraps negative once expired).
#[no_mangle]
pub extern "C" fn fepor_mfdec() -> u32 {
    let m = machine();
    match m.dec_deadline {
        Some(d) => d.wrapping_sub(m.timebase()) as u32,
        None => 0xFFFF_FFFF,
    }
}

#[no_mangle]
pub extern "C" fn fepor_hw_read(ea: u32, size: i32) -> u32 {
    machine()
        .hw
        .read(ea, size as u32)
        .unwrap_or_else(|e| die_str(&e))
}

#[no_mangle]
pub extern "C" fn fepor_hw_write(ea: u32, value: u32, size: i32) {
    let m = machine();
    m.hw.write(ea, value, size as u32, &mut m.ram)
        .unwrap_or_else(|e| die_str(&e))
}

#[no_mangle]
pub extern "C" fn fepor_enter(addr: u32) {
    let m = machine();
    if m.trace.calls {
        m.trace.enter(addr);
    }
    if !m.trace.watch.is_empty() && m.trace.watch.contains(&addr) {
        // SAFETY: the running thread's context.
        let c = unsafe { &*crate::sched::current_context() };
        eprintln!(
            "watch: {addr:08x} thread#{} r3={:08x} r4={:08x} r5={:08x} r6={:08x} lr={:08x} f1..f4={} {} {} {}",
            crate::sched::current(),
            c.r[3],
            c.r[4],
            c.r[5],
            c.r[6],
            c.lr,
            c.f[1].ps0,
            c.f[2].ps0,
            c.f[3].ps0,
            c.f[4].ps0
        );
    }
    // Function entry is the safe point where interrupts are delivered (registers are synced here).
    m.enter_count = m.enter_count.wrapping_add(1);
    if m.enter_count & 0xFF == 0 {
        crate::intr::poll(crate::sched::current_context());
        // (function entries poll with the thread's context; loops pass their own)
    }
}

/// Backward-branch countdown shared with the generated code.
#[no_mangle]
pub static mut fepor_ticks: u32 = POLL_INTERVAL;
/// Backward branches between interrupt polls. Interrupts follow wall-clock time, so the interval must stay
/// short enough for the slowest guest loop that waits on one (the compiled-in-process code is slower per
/// branch than the clang build: at 4096 the boot stalled in two threads yielding to each other).
const POLL_INTERVAL: u32 = 256;

/// Safe point reached from a loop in generated code.
#[no_mangle]
pub extern "C" fn fepor_poll(c: *mut Context) {
    // SAFETY: single guest thread at a time; the generated code passes its live context.
    unsafe { fepor_ticks = POLL_INTERVAL };
    crate::intr::poll(c);
}

#[cfg(feature = "dispatch")]
mod dispatch {
    use super::*;

    #[repr(C)]
    pub struct Entry {
        addr: u32,
        func: HostFn,
    }

    extern "C" {
        static fepor_dispatch_table: [Entry; 0];
        static fepor_dispatch_count: usize;
    }

    /// Guest address to generated function.
    pub fn find(target: u32) -> Option<HostFn> {
        // SAFETY: the generated dispatch.cpp defines a sorted table of `fepor_dispatch_count` entries.
        let table = unsafe {
            std::slice::from_raw_parts(fepor_dispatch_table.as_ptr(), fepor_dispatch_count)
        };
        table
            .binary_search_by_key(&target, |e| e.addr)
            .ok()
            .map(|i| table[i].func)
    }

    #[no_mangle]
    pub extern "C" fn fepor_lookup(from: u32, target: u32) -> HostFn {
        find(target).unwrap_or_else(|| {
            die_str(&format!(
                "indirect branch at {from:08x} to {target:08x}: not in dispatch table"
            ))
        })
    }
}

/// The host function of guest address `target` (see `guest_function`).
pub fn find_function(target: u32) -> Option<HostFn> {
    guest_function(target)
}

/// Addresses of every exported function, so a binary can force the linker to keep them for the
/// generated static library (which references them but comes later on the link line).
pub fn exports() -> Vec<usize> {
    use crate::hle::{fepor_hle_OSLoadContext, fepor_hle_OSSaveContext};
    #[allow(unused_mut)]
    let mut v = vec![
        fepor_unimplemented as *const () as usize,
        fepor_unresolved as *const () as usize,
        fepor_trap as *const () as usize,
        fepor_syscall as *const () as usize,
        fepor_rfi as *const () as usize,
        fepor_mftb as *const () as usize,
        fepor_mtdec as *const () as usize,
        fepor_lc_dma as *const () as usize,
        fepor_mfdec as *const () as usize,
        fepor_hw_read as *const () as usize,
        fepor_hw_write as *const () as usize,
        fepor_enter as *const () as usize,
        fepor_poll as *const () as usize,
        std::ptr::addr_of!(fepor_ticks) as usize,
        fepor_hle_OSLoadContext as *const () as usize,
        fepor_hle_OSSaveContext as *const () as usize,
    ];
    #[cfg(feature = "dispatch")]
    v.push(dispatch::fepor_lookup as *const () as usize);
    v
}
