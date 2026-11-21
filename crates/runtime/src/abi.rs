//! The C ABI the generated code links against (declared in `runtime/include/fepor/ppc.h`).

use crate::context::Context;
use crate::machine::machine;

pub type HostFn = unsafe extern "C" fn(*mut Context);

static CURRENT: std::sync::atomic::AtomicPtr<Context> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Register the context of the running guest thread (used for crash reports).
pub fn set_current(c: *mut Context) {
    CURRENT.store(c, std::sync::atomic::Ordering::Relaxed);
}

/// `FEPOR_WATCHDOG=<seconds>`: after that long, print where the guest is (recent calls, registers) and
/// exit with status 124. Meant for finding spin loops while bringing the boot up.
pub fn start_watchdog() {
    let Some(secs) = std::env::var("FEPOR_WATCHDOG").ok().and_then(|v| v.parse::<u64>().ok()) else { return };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(secs));
        eprintln!("fepor: watchdog: guest still running after {secs}s");
        // The guest thread keeps running while we read its state: good enough for a crash report.
        let c = CURRENT.load(std::sync::atomic::Ordering::Relaxed);
        if !c.is_null() {
            // SAFETY: set_current stores a context that outlives the guest run.
            let c = unsafe { &*c };
            eprintln!("  r1={:#010x} lr={:#010x} ctr={:#010x} r3={:#010x} msr={:#06x}", c.r[1], c.lr, c.ctr, c.r[3], c.msr);
        }
        let recent = machine().trace.recent(32);
        eprintln!("  recent calls (oldest first): {}", recent.iter().map(|a| format!("{a:08x}")).collect::<Vec<_>>().join(" "));
        std::process::exit(124);
    });
}

/// Report a fatal guest error with state and recent calls, then exit with status 70.
pub fn die_str(msg: &str) -> ! {
    eprintln!("fepor: fatal: {msg}");
    let c = CURRENT.load(std::sync::atomic::Ordering::Relaxed);
    if !c.is_null() {
        // SAFETY: set_current stores a context that outlives the guest run.
        let c = unsafe { &*c };
        eprintln!("  r1={:#010x} lr={:#010x} ctr={:#010x}", c.r[1], c.lr, c.ctr);
        eprintln!("  r3={:#010x} r4={:#010x} r5={:#010x} r6={:#010x}", c.r[3], c.r[4], c.r[5], c.r[6]);
    }
    let recent = machine().trace.recent(32);
    if !recent.is_empty() {
        eprintln!("  recent calls (oldest first): {}", recent.iter().map(|a| format!("{a:08x}")).collect::<Vec<_>>().join(" "));
    }
    std::process::exit(70);
}

#[no_mangle]
pub extern "C" fn fepor_unimplemented(_c: *mut Context, addr: u32, code: u32) -> ! {
    die_str(&format!("unimplemented instruction {code:08x} at {addr:08x}"))
}

#[no_mangle]
pub extern "C" fn fepor_unresolved(_c: *mut Context, from: u32, target: u32) -> ! {
    die_str(&format!("control flow from {from:08x} to {target:08x} has no generated function"))
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
        None => die_str(&format!("rfi at {addr:08x} to {srr0:#010x}: no generated function")),
    }
}

const SRR0: usize = 26;

#[cfg(feature = "dispatch")]
pub fn guest_function(target: u32) -> Option<HostFn> {
    dispatch::find(target)
}

#[cfg(not(feature = "dispatch"))]
pub fn guest_function(_target: u32) -> Option<HostFn> {
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

#[no_mangle]
pub extern "C" fn fepor_hw_read(ea: u32, size: i32) -> u32 {
    machine().hw.read(ea, size as u32).unwrap_or_else(|e| die_str(&e))
}

#[no_mangle]
pub extern "C" fn fepor_hw_write(ea: u32, value: u32, size: i32) {
    let m = machine();
    m.hw.write(ea, value, size as u32, &mut m.ram).unwrap_or_else(|e| die_str(&e))
}

#[no_mangle]
pub extern "C" fn fepor_enter(addr: u32) {
    let m = machine();
    if m.trace.calls {
        m.trace.enter(addr);
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
const POLL_INTERVAL: u32 = 4096;

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
        let table = unsafe { std::slice::from_raw_parts(fepor_dispatch_table.as_ptr(), fepor_dispatch_count) };
        table.binary_search_by_key(&target, |e| e.addr).ok().map(|i| table[i].func)
    }

    #[no_mangle]
    pub extern "C" fn fepor_lookup(from: u32, target: u32) -> HostFn {
        find(target).unwrap_or_else(|| die_str(&format!("indirect branch at {from:08x} to {target:08x}: not in dispatch table")))
    }
}

#[cfg(feature = "dispatch")]
pub use dispatch::find as find_function;

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
