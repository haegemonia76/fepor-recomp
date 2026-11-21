//! External interrupt delivery.
//!
//! At a safe point (function entry) with MSR[EE] set and an unmasked interrupt pending, the runtime
//! does what the exception vector stub at 0x80000500 does and runs the SDK's exception handler:
//! r3 = 0x500, r2 = "SRR0", r4 = "SRR1", the interrupted r2-r4 in SPRG1-3. SRR0 is made equal to LR so
//! that the handler's closing `rfi` returns here (see `fepor_rfi`). If the handler switches threads,
//! this thread is resumed later by unwinding back to this function (`sched::ParkKind::AfterIrq`).

use crate::abi::{die_str, guest_function};
use crate::context::Context;
use crate::machine::machine;
use crate::sched;

const EXT_VECTOR: u32 = 0x8000_0500;
const MSR_EE: u32 = 0x8000;
const CURRENT_CONTEXT: u32 = 0x8000_00D4;

const OFF_STATE: u32 = 0x1A2;
const STATE_EXC: u16 = 2;
const MSR_RI: u32 = 0x2;
/// Physical address of the current OSContext, stored by the SDK at 0x800000C0.
const CURRENT_CONTEXT_PHYS: u32 = 0x8000_00C0;
const EXCEPTION_TABLE: u32 = 0x8000_3000;

/// The exception number the OS patched into the external-interrupt vector copy (`li r3,N`).
fn vector_number() -> Option<u32> {
    let m = machine();
    (0..40)
        .map(|i| m.read_u32(EXT_VECTOR + 4 * i))
        .find(|&w| w & 0xFFFF_0000 == 0x3860_0000)
        .map(|w| w & 0xFFFF)
}

/// Called from `fepor_enter` with the running thread's context.
pub fn poll(c: *mut Context) {
    // SAFETY: the running thread's context.
    let c = unsafe { &mut *c };
    if machine().trace.irq && machine().enter_count % (256 * 2000) == 0 {
        eprintln!(
            "irq: poll msr={:#x} pending={}",
            c.msr,
            machine().hw.interrupt_pending()
        );
    }
    if c.msr & MSR_EE == 0 {
        return;
    }
    let m = machine();
    if !m.hw.interrupt_pending() {
        return;
    }
    let Some(number) = vector_number() else {
        return;
    }; // vectors not installed yet
    if m.trace.irq {
        eprintln!(
            "irq: delivering exception {number} (msr {:#x}, lr {:#x})",
            c.msr, c.lr
        );
    }
    deliver(c, number);
}

/// Do what the vector stub at 0x80000500 does, then run the OS handler it dispatches to.
fn deliver(c: &mut Context, number: u32) {
    let m = machine();
    let ctx_virtual = m.read_u32(CURRENT_CONTEXT);
    let ctx = 0x8000_0000 | m.read_u32(CURRENT_CONTEXT_PHYS);
    let me = sched::current();

    // SAFETY: Context is plain data; the copy is restored below.
    let saved: Box<Context> = unsafe {
        let mut b = Box::<Context>::new_uninit();
        std::ptr::copy_nonoverlapping(c as *const Context, b.as_mut_ptr(), 1);
        b.assume_init()
    };

    // Stub prologue: save what it clobbers and the exception state into the current context.
    m.write_u32(ctx + 0xC, c.r[3]);
    m.write_u32(ctx + 0x10, c.r[4]);
    m.write_u32(ctx + 0x14, c.r[5]);
    let state = u16::from_be_bytes([
        m.ram[(ctx + OFF_STATE) as usize & 0x3FFF_FFFF],
        m.ram[(ctx + OFF_STATE + 1) as usize & 0x3FFF_FFFF],
    ]) | STATE_EXC;
    m.ram[(ctx + OFF_STATE) as usize & 0x3FFF_FFFF..][..2].copy_from_slice(&state.to_be_bytes());
    m.write_u32(ctx + 0x80, crate::context::pack_cr(&c.cr));
    m.write_u32(ctx + 0x84, c.lr);
    m.write_u32(ctx + 0x88, c.ctr);
    m.write_u32(ctx + 0x8C, crate::context::pack_xer(&c.xer));
    m.write_u32(ctx + 0x198, c.lr);
    m.write_u32(ctx + 0x19C, c.msr);

    // With MSR[RI] set the stub jumps through the OS exception handler table.
    let handler = if c.msr & MSR_RI != 0 {
        m.read_u32(EXCEPTION_TABLE + ((number & 0xFF) << 2))
    } else {
        0
    };
    let Some(f) = guest_function(handler) else {
        die_str(&format!(
            "exception {number}: handler {handler:#010x} has no generated function"
        ))
    };

    let outer = sched::irq_ctx(me);
    if outer.is_some() && m.trace.irq {
        eprintln!(
            "irq: nested delivery inside the handler of context {:#x}",
            outer.unwrap()
        );
    }
    sched::set_irq_ctx(me, Some(ctx_virtual));
    c.r[5] = c.msr; // SRR1
    c.r[3] = number;
    c.r[4] = ctx_virtual;
    c.msr = (c.msr & !MSR_EE) | 0x30;
    let unwound = crate::hle::run(f, c);
    sched::set_irq_ctx(me, outer);

    if !unwound {
        // The handler returned normally: the interrupted code continues with its own registers.
        // SAFETY: same type, plain data.
        unsafe { std::ptr::copy_nonoverlapping(&*saved as *const Context, c as *mut Context, 1) };
    }
    // else: this thread was resumed by OSLoadContext, which already restored the registers.
}
