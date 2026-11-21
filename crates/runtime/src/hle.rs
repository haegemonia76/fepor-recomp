//! Runtime replacements for SDK functions (`[[hle]]` entries in the version data, ADR 0012).
//!
//! The OSContext layout (offsets) follows the SDK: gpr[32] at 0x00, cr 0x80, lr 0x84, ctr 0x88,
//! xer 0x8C, srr0 0x198, srr1 0x19C, state 0x1A2, gqr[8] at 0x1A4.

use crate::abi::{die_str, guest_function};
use crate::context::Context;
use crate::machine::machine;
use crate::sched::{self, ParkKind};

const OFF_CR: u32 = 0x80;
const OFF_LR: u32 = 0x84;
const OFF_CTR: u32 = 0x88;
const OFF_XER: u32 = 0x8C;
const OFF_SRR0: u32 = 0x198;
const OFF_SRR1: u32 = 0x19C;
const OFF_STATE: u32 = 0x1A2;
const OFF_GQR: u32 = 0x1A4;
const STATE_EXC: u16 = 2;
const SPR_GQR0: usize = 912;
/// `c.spr[HLE_STATUS]` tells the generated call site what happened after OSLoadContext returns.
const HLE_STATUS: usize = 1023;

fn rd32(addr: u32) -> u32 {
    machine().read_u32(addr)
}

fn wr32(addr: u32, v: u32) {
    machine().write_u32(addr, v);
}

fn rd16(addr: u32) -> u16 {
    let m = machine();
    let p = (addr & 0x3FFF_FFFF) as usize;
    u16::from_be_bytes([m.ram[p], m.ram[p + 1]])
}

fn wr16(addr: u32, v: u16) {
    let m = machine();
    let p = (addr & 0x3FFF_FFFF) as usize;
    m.ram[p..p + 2].copy_from_slice(&v.to_be_bytes());
}

/// OSSaveContext(context): store the registers the SDK stores, return 0 (a resumed thread sees 1).
#[no_mangle]
pub extern "C" fn fepor_hle_OSSaveContext(c: *mut Context) {
    // SAFETY: the generated stub passes its live context.
    let c = unsafe { &mut *c };
    let ctx = c.r[3];
    for i in 13..32 {
        wr32(ctx + 4 * i as u32, c.r[i]);
    }
    for i in 1..8 {
        wr32(ctx + OFF_GQR + 4 * i as u32, c.spr[SPR_GQR0 + i]);
    }
    wr32(ctx + OFF_CR, crate::context::pack_cr(&c.cr));
    wr32(ctx + OFF_LR, c.lr);
    wr32(ctx + OFF_SRR0, c.lr);
    wr32(ctx + OFF_SRR1, c.msr);
    wr32(ctx + OFF_CTR, c.ctr);
    wr32(ctx + OFF_XER, crate::context::pack_xer(&c.xer));
    wr32(ctx + 4, c.r[1]);
    wr32(ctx + 8, c.r[2]);
    wr32(ctx + 0xC, 1); // the value r3 has when the context is loaded
    sched::set_last_save(sched::current(), ctx);
    c.r[3] = 0;
}

/// Load the registers an OSLoadContext would restore. Exception contexts restore everything.
fn restore(c: &mut Context, ctx: u32) {
    c.r[0] = rd32(ctx);
    c.r[1] = rd32(ctx + 4);
    c.r[2] = rd32(ctx + 8);
    let state = rd16(ctx + OFF_STATE);
    let first = if state & STATE_EXC != 0 {
        wr16(ctx + OFF_STATE, state & !STATE_EXC);
        5
    } else {
        13
    };
    for i in first..32 {
        c.r[i] = rd32(ctx + 4 * i as u32);
    }
    for i in 1..8 {
        c.spr[SPR_GQR0 + i] = rd32(ctx + OFF_GQR + 4 * i as u32);
    }
    crate::context::unpack_cr(&mut c.cr, rd32(ctx + OFF_CR));
    c.lr = rd32(ctx + OFF_LR);
    c.ctr = rd32(ctx + OFF_CTR);
    c.xer = crate::context::unpack_xer(rd32(ctx + OFF_XER));
    c.msr = rd32(ctx + OFF_SRR1);
    c.r[4] = rd32(ctx + 0x10);
    c.r[3] = rd32(ctx + 0xC);
}

/// OSLoadContext(context): switch to the thread that owns `context`.
#[no_mangle]
pub extern "C" fn fepor_hle_OSLoadContext(c: *mut Context) {
    // SAFETY: the generated stub passes its live context.
    let c = unsafe { &mut *c };
    let target = c.r[3];
    let me = sched::current();
    let token = sched::token(me);
    let in_irq = matches!(token, Some((_, ParkKind::AfterIrq)));

    if token.is_some_and(|(t, _)| t == target) {
        // The thread picked itself again: continue where it saved (or where it was interrupted).
        restore(c, target);
        c.spr[HLE_STATUS] = in_irq as u32;
        return;
    }

    if let Some(other) = sched::parked_at(target) {
        sched::switch_to(me, other, token);
    } else {
        // A context nobody has run from yet: a new thread starting at its saved PC.
        let ram = machine().ram_ptr();
        let mut ctx = Context::new(ram);
        restore(&mut ctx, target);
        let entry = rd32(target + OFF_SRR0);
        sched::spawn_and_switch(me, token, ctx, move |cp| {
            // SAFETY: the context was boxed for this thread and is only used by it.
            let cp = unsafe { &mut *cp };
            let Some(f) = guest_function(entry) else {
                die_str(&format!(
                    "new thread entry {entry:#010x} (context {target:#010x}) has no generated function"
                ))
            };
            run(f, cp);
            // thread functions return into the SDK's exit stub (OSExitThread)
            let ret = cp.lr;
            if let Some(f) = guest_function(ret) {
                run(f, cp);
            }
        });
    }

    // Resumed by someone else's OSLoadContext.
    match sched::woken_as(me) {
        Some((ctx, kind)) => {
            restore(c, ctx);
            c.spr[HLE_STATUS] = (kind == ParkKind::AfterIrq) as u32;
        }
        None => die_str("a thread without a context was resumed"),
    }
}

extern "C" {
    fn fepor_call_unwindable(f: crate::abi::HostFn, c: *mut Context) -> i32;
}

/// Run a generated function; returns true if the host stack was unwound (`fepor::Unwind`).
pub fn run(f: crate::abi::HostFn, c: &mut Context) -> bool {
    // SAFETY: f is a generated function; the shim only catches fepor::Unwind.
    unsafe { fepor_call_unwindable(f, c) == 1 }
}
