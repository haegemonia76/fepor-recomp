//! Floating-point and paired-single instructions, executed out of line on the guest `Context`.
//!
//! These are the semantics of `runtime/include/fepor/ppc.h` (the C++ backend), written in Rust: a NaN
//! operand is returned quieted (priority A, B, C), an invalid operation yields the default NaN, single
//! precision results are rounded, `fmuls`-like instructions force the multiplier to 25 bits. Floating-point
//! registers live in the context (not in JIT variables), so none of this needs the integer state in sync.

use fepor_runtime::abi::{fepor_hw_read, fepor_hw_write};
use fepor_runtime::Context;
use ppc750cl::{Ins, Opcode};

extern "C" {
    fn fepor_fres(v: f64) -> f64;
    fn fepor_frsqrte(v: f64) -> f64;
}

const RAM_SIZE: u32 = 0x0180_0000;

// ---- memory (the same fast path / hardware path split as the generated code) ----------------------------

fn in_ram(ea: u32) -> bool {
    (ea & 0x3FFF_FFFF) < RAM_SIZE
}

unsafe fn ptr(c: &Context, ea: u32) -> *mut u8 {
    c.ram.add((ea & 0x3FFF_FFFF) as usize)
}

unsafe fn ld8(c: &Context, ea: u32) -> u32 {
    if in_ram(ea) {
        *ptr(c, ea) as u32
    } else {
        fepor_hw_read(ea, 1) & 0xFF
    }
}
unsafe fn ld16(c: &Context, ea: u32) -> u32 {
    if in_ram(ea) {
        u16::from_be((ptr(c, ea) as *const u16).read_unaligned()) as u32
    } else {
        fepor_hw_read(ea, 2) & 0xFFFF
    }
}
unsafe fn ld32(c: &Context, ea: u32) -> u32 {
    if in_ram(ea) {
        u32::from_be((ptr(c, ea) as *const u32).read_unaligned())
    } else {
        fepor_hw_read(ea, 4)
    }
}
unsafe fn ld64(c: &Context, ea: u32) -> u64 {
    if in_ram(ea) {
        u64::from_be((ptr(c, ea) as *const u64).read_unaligned())
    } else {
        (fepor_hw_read(ea, 4) as u64) << 32 | fepor_hw_read(ea.wrapping_add(4), 4) as u64
    }
}
unsafe fn st8(c: &Context, ea: u32, v: u32) {
    if in_ram(ea) {
        *ptr(c, ea) = v as u8;
    } else {
        fepor_hw_write(ea, v & 0xFF, 1);
    }
}
unsafe fn st16(c: &Context, ea: u32, v: u32) {
    if in_ram(ea) {
        (ptr(c, ea) as *mut u16).write_unaligned((v as u16).to_be());
    } else {
        fepor_hw_write(ea, v & 0xFFFF, 2);
    }
}
unsafe fn st32(c: &Context, ea: u32, v: u32) {
    if in_ram(ea) {
        (ptr(c, ea) as *mut u32).write_unaligned(v.to_be());
    } else {
        fepor_hw_write(ea, v, 4);
    }
}
unsafe fn st64(c: &Context, ea: u32, v: u64) {
    if in_ram(ea) {
        (ptr(c, ea) as *mut u64).write_unaligned(v.to_be());
    } else {
        fepor_hw_write(ea, (v >> 32) as u32, 4);
        fepor_hw_write(ea.wrapping_add(4), v as u32, 4);
    }
}

// ---- conversions and arithmetic rules ------------------------------------------------------------------

fn round_single(x: f64) -> f64 {
    x as f32 as f64
}

fn force25(d: f64) -> f64 {
    let b = d.to_bits();
    if (b >> 52) & 0x7FF == 0x7FF {
        return d;
    }
    let integral = b & 0xFFFF_FFFF_F800_0000;
    f64::from_bits(integral.wrapping_add(b & 0x0800_0000))
}

fn quiet_nan(x: f64) -> f64 {
    f64::from_bits(x.to_bits() | 0x0008_0000_0000_0000)
}
fn default_nan() -> f64 {
    f64::from_bits(0x7FF8_0000_0000_0000)
}
fn nan_fix2(r: f64, a: f64, b: f64) -> f64 {
    if !r.is_nan() {
        r
    } else if a.is_nan() {
        quiet_nan(a)
    } else if b.is_nan() {
        quiet_nan(b)
    } else {
        default_nan()
    }
}
fn nan_fix3(r: f64, a: f64, b: f64, c: f64) -> f64 {
    if !r.is_nan() {
        r
    } else if a.is_nan() {
        quiet_nan(a)
    } else if b.is_nan() {
        quiet_nan(b)
    } else if c.is_nan() {
        quiet_nan(c)
    } else {
        default_nan()
    }
}
fn pfadd(a: f64, b: f64) -> f64 {
    nan_fix2(a + b, a, b)
}
fn pfsub(a: f64, b: f64) -> f64 {
    nan_fix2(a - b, a, b)
}
fn pfmul(a: f64, c: f64) -> f64 {
    nan_fix2(a * c, a, c)
}
fn pfdiv(a: f64, b: f64) -> f64 {
    nan_fix2(a / b, a, b)
}
fn pfmadd(a: f64, c: f64, b: f64) -> f64 {
    nan_fix3(a.mul_add(c, b), a, b, c)
}
fn pfmsub(a: f64, c: f64, b: f64) -> f64 {
    nan_fix3(a.mul_add(c, -b), a, b, c)
}
fn neg_nonan(x: f64) -> f64 {
    if x.is_nan() {
        x
    } else {
        -x
    }
}

fn fctiw(b: f64, mode: u32) -> f64 {
    let rounded = match mode & 3 {
        0 => b.round_ties_even(),
        1 => b.trunc(),
        2 => b.ceil(),
        _ => b.floor(),
    };
    let value: u32 = if b.is_nan() {
        0x8000_0000
    } else if rounded >= 2147483648.0 {
        0x7FFF_FFFF
    } else if rounded < -2147483648.0 {
        0x8000_0000
    } else {
        rounded as i32 as u32
    };
    let mut result = 0xFFF8_0000_0000_0000u64 | value as u64;
    if value == 0 && b.is_sign_negative() {
        result |= 0x1_0000_0000;
    }
    f64::from_bits(result)
}

fn fsel(a: f64, b: f64, c: f64) -> f64 {
    if a >= -0.0 {
        c
    } else {
        b
    }
}

/// CR field bits (lt, gt, eq, so) of an FP compare.
fn fcmp(a: f64, b: f64) -> [u8; 4] {
    if a.is_nan() || b.is_nan() {
        [0, 0, 0, 1]
    } else {
        [(a < b) as u8, (a > b) as u8, (a == b) as u8, 0]
    }
}

fn single_to_double(x: u32) -> f64 {
    if (x >> 23) & 0xFF == 0xFF {
        return f64::from_bits(
            ((x & 0x8000_0000) as u64) << 32
                | 0x7FF0_0000_0000_0000
                | ((x & 0x7F_FFFF) as u64) << 29,
        );
    }
    f32::from_bits(x) as f64
}

fn double_to_single(d: f64) -> u32 {
    let x = d.to_bits();
    let exp = ((x >> 52) & 0x7FF) as u32;
    if exp > 896 || x & 0x7FFF_FFFF_FFFF_FFFF == 0 {
        return ((x >> 32) as u32 & 0xC000_0000) | ((x >> 29) as u32 & 0x3FFF_FFFF);
    }
    if exp >= 874 {
        let mut t = (x & 0x000F_FFFF_FFFF_FFFF) | 0x0010_0000_0000_0000;
        t >>= 926 - exp;
        return ((x >> 32) as u32 & 0x8000_0000) | t as u32;
    }
    (x >> 32) as u32 & 0x8000_0000
}

fn pow2(e: i32) -> f64 {
    // exact for the quantization scales (-32..31)
    f64::from_bits(((1023 + e) as u64) << 52)
}

unsafe fn dequant(c: &Context, ty: u32, scale: i32, ea: u32) -> (f64, u32) {
    let (v, size) = match ty {
        4 => (ld8(c, ea) as f64, 1),
        5 => (ld16(c, ea) as f64, 2),
        6 => (ld8(c, ea) as u8 as i8 as f64, 1),
        7 => (ld16(c, ea) as u16 as i16 as f64, 2),
        _ => return (single_to_double(ld32(c, ea)), 4),
    };
    (v * pow2(-scale), size)
}

unsafe fn psq_ld(c: &Context, ea: u32, gqr: u32, w: bool) -> (f64, f64) {
    let mut ty = gqr >> 16 & 7;
    let mut scale = (gqr >> 24 & 0x3F) as i32;
    scale = if scale >= 32 { scale - 64 } else { scale };
    if ty < 4 || ty == 1 || ty == 2 || ty == 3 {
        ty = 0;
        scale = 0;
    }
    let (a, sz) = dequant(c, ty, scale, ea);
    if w {
        return (a, 1.0);
    }
    (a, dequant(c, ty, scale, ea.wrapping_add(sz)).0)
}

fn quant(v: f64, ty: u32, scale: i32) -> (u32, u32) {
    if !(4..=7).contains(&ty) {
        return (double_to_single(v), 4);
    }
    let v = v * pow2(scale);
    match ty {
        4 => (
            if v <= 0.0 || v.is_nan() {
                0
            } else if v >= 255.0 {
                255
            } else {
                v as i32 as u32
            },
            1,
        ),
        5 => (
            if v <= 0.0 || v.is_nan() {
                0
            } else if v >= 65535.0 {
                65535
            } else {
                v as i32 as u32
            },
            2,
        ),
        6 => (
            (if v.is_nan() {
                0
            } else if v <= -128.0 {
                -128
            } else if v >= 127.0 {
                127
            } else {
                v as i32
            }) as u32
                & 0xFF,
            1,
        ),
        _ => (
            (if v.is_nan() {
                0
            } else if v <= -32768.0 {
                -32768
            } else if v >= 32767.0 {
                32767
            } else {
                v as i32
            }) as u32
                & 0xFFFF,
            2,
        ),
    }
}

unsafe fn psq_st(c: &Context, ea: u32, gqr: u32, w: bool, v: (f64, f64)) {
    let ty = gqr & 7;
    let mut scale = (gqr >> 8 & 0x3F) as i32;
    scale = if scale >= 32 { scale - 64 } else { scale };
    let put = |at: u32, d: f64| -> u32 {
        let (q, sz) = quant(d, ty, scale);
        match sz {
            1 => st8(c, at, q),
            2 => st16(c, at, q),
            _ => st32(c, at, q),
        }
        sz
    };
    let sz = put(ea, v.0);
    if !w {
        put(ea.wrapping_add(sz), v.1);
    }
}

// ---- the instructions ---------------------------------------------------------------------------------

fn f0(c: &Context, n: u8) -> f64 {
    c.f[n as usize].ps0
}
fn f1(c: &Context, n: u8) -> f64 {
    c.f[n as usize].ps1
}
fn set(c: &mut Context, n: u8, a: f64, b: f64) {
    c.f[n as usize].ps0 = a;
    c.f[n as usize].ps1 = b;
}
fn set_both(c: &mut Context, n: u8, a: f64) {
    set(c, n, a, a);
}

/// Decoding an instruction word (a table walk) costs more than executing many of these: remember the
/// last ones, direct-mapped by word. Only the guest thread holding the baton gets here.
fn decode(word: u32) -> Ins {
    static mut CACHE: [(u32, Option<Ins>); 2048] = [(0, None); 2048];
    let slot = ((word ^ word >> 11) & 2047) as usize;
    // SAFETY: one guest thread runs at a time (the scheduler's baton), and the helpers do not nest.
    unsafe {
        let cache = &mut *std::ptr::addr_of_mut!(CACHE);
        match cache[slot] {
            (w, Some(ins)) if w == word => ins,
            _ => {
                let ins = Ins::new(word);
                cache[slot] = (word, Some(ins));
                ins
            }
        }
    }
}

/// CR1 after a record-form FP instruction: the FPSCR exception summary bits.
fn rc1(c: &mut Context, ins: Ins) {
    if ins.field_rc() {
        let f = c.fpscr;
        c.cr[1].lt = (f >> 31 & 1) as u8;
        c.cr[1].gt = (f >> 30 & 1) as u8;
        c.cr[1].eq = (f >> 29 & 1) as u8;
        c.cr[1].so = (f >> 28 & 1) as u8;
    }
}

fn set_cr(c: &mut Context, n: u8, v: [u8; 4]) {
    let f = &mut c.cr[n as usize];
    (f.lt, f.gt, f.eq, f.so) = (v[0], v[1], v[2], v[3]);
}

/// Which CR field an FP instruction writes (the JIT reloads it from the context afterwards).
pub fn cr_field_written(ins: Ins) -> Option<u8> {
    use Opcode::*;
    match ins.op {
        Fcmpu | Fcmpo | PsCmpu0 | PsCmpo0 | PsCmpu1 | PsCmpo1 | Mcrfs => Some(ins.field_crfd()),
        _ if ins.field_rc() => Some(1),
        _ => None,
    }
}

/// Execute one floating-point or paired-single instruction that touches no integer register or memory.
///
/// # Safety
/// `c` must be a live guest context.
pub unsafe extern "C" fn fepor_jit_fp(c: *mut Context, word: u32) {
    use Opcode::*;
    let c = &mut *c;
    let ins = decode(word);
    let (fd, fa, fb, fc) = (
        ins.field_frd(),
        ins.field_fra(),
        ins.field_frb(),
        ins.field_frc(),
    );
    match ins.op {
        Fmr | Fabs | Fneg | Fnabs => {
            let y = f0(c, fb);
            let v = match ins.op {
                Fmr => y,
                Fabs => y.abs(),
                Fneg => -y,
                _ => -y.abs(),
            };
            c.f[fd as usize].ps0 = v;
            rc1(c, ins);
        }
        Fadd | Fsub | Fmul | Fdiv | Fadds | Fsubs | Fmuls | Fdivs => {
            let (a, b, cc) = (f0(c, fa), f0(c, fb), f0(c, fc));
            let (e, single) = match ins.op {
                Fadd => (pfadd(a, b), false),
                Fsub => (pfsub(a, b), false),
                Fmul => (pfmul(a, cc), false),
                Fdiv => (pfdiv(a, b), false),
                Fadds => (pfadd(a, b), true),
                Fsubs => (pfsub(a, b), true),
                Fmuls => (pfmul(a, force25(cc)), true),
                _ => (pfdiv(a, b), true),
            };
            if single {
                set_both(c, fd, round_single(e));
            } else {
                c.f[fd as usize].ps0 = e;
            }
            rc1(c, ins);
        }
        Fmadd | Fmsub | Fnmadd | Fnmsub | Fmadds | Fmsubs | Fnmadds | Fnmsubs => {
            let (a, b, cc) = (f0(c, fa), f0(c, fb), f0(c, fc));
            let single = matches!(ins.op, Fmadds | Fmsubs | Fnmadds | Fnmsubs);
            let cc = if single { force25(cc) } else { cc };
            let (sub, neg) = match ins.op {
                Fmadd | Fmadds => (false, false),
                Fmsub | Fmsubs => (true, false),
                Fnmadd | Fnmadds => (false, true),
                _ => (true, true),
            };
            let mut e = if sub {
                pfmsub(a, cc, b)
            } else {
                pfmadd(a, cc, b)
            };
            if single {
                e = round_single(e);
            }
            if neg {
                e = neg_nonan(e);
            }
            if single {
                set_both(c, fd, e);
            } else {
                c.f[fd as usize].ps0 = e;
            }
            rc1(c, ins);
        }
        Fres => {
            let v = fepor_fres(f0(c, fb));
            set_both(c, fd, v);
            rc1(c, ins);
        }
        Frsqrte => {
            let v = fepor_frsqrte(f0(c, fb));
            c.f[fd as usize].ps0 = v;
            rc1(c, ins);
        }
        Frsp => {
            let v = round_single(f0(c, fb));
            set_both(c, fd, v);
            rc1(c, ins);
        }
        Fsel => {
            let v = fsel(f0(c, fa), f0(c, fb), f0(c, fc));
            c.f[fd as usize].ps0 = v;
            rc1(c, ins);
        }
        Fctiw | Fctiwz => {
            let mode = if ins.op == Fctiwz { 1 } else { c.fpscr & 3 };
            let v = fctiw(f0(c, fb), mode);
            c.f[fd as usize].ps0 = v;
            rc1(c, ins);
        }
        Fcmpu | Fcmpo => {
            let v = fcmp(f0(c, fa), f0(c, fb));
            set_cr(c, ins.field_crfd(), v);
        }
        PsAdd | PsSub | PsMul | PsDiv | PsMadd | PsMsub | PsNmadd | PsNmsub => {
            let lane = |a: f64, b: f64, cc: f64| -> f64 {
                match ins.op {
                    PsAdd => round_single(pfadd(a, b)),
                    PsSub => round_single(pfsub(a, b)),
                    PsMul => round_single(pfmul(a, force25(cc))),
                    PsDiv => round_single(pfdiv(a, b)),
                    PsMadd => round_single(pfmadd(a, force25(cc), b)),
                    PsMsub => round_single(pfmsub(a, force25(cc), b)),
                    PsNmadd => neg_nonan(round_single(pfmadd(a, force25(cc), b))),
                    _ => neg_nonan(round_single(pfmsub(a, force25(cc), b))),
                }
            };
            let x = lane(f0(c, fa), f0(c, fb), f0(c, fc));
            let y = lane(f1(c, fa), f1(c, fb), f1(c, fc));
            set(c, fd, x, y);
            rc1(c, ins);
        }
        PsMuls0 | PsMuls1 | PsMadds0 | PsMadds1 => {
            let s = if matches!(ins.op, PsMuls0 | PsMadds0) {
                f0(c, fc)
            } else {
                f1(c, fc)
            };
            let mul = matches!(ins.op, PsMuls0 | PsMuls1);
            let lane = |a: f64, b: f64| -> f64 {
                if mul {
                    round_single(pfmul(a, force25(s)))
                } else {
                    round_single(pfmadd(a, force25(s), b))
                }
            };
            let x = lane(f0(c, fa), f0(c, fb));
            let y = lane(f1(c, fa), f1(c, fb));
            set(c, fd, x, y);
            rc1(c, ins);
        }
        PsSum0 | PsSum1 => {
            let sum = round_single(pfadd(f0(c, fa), f1(c, fb)));
            let (x, y) = if ins.op == PsSum0 {
                (sum, f1(c, fc))
            } else {
                (f0(c, fc), sum)
            };
            set(c, fd, x, y);
            rc1(c, ins);
        }
        PsMerge00 | PsMerge01 | PsMerge10 | PsMerge11 => {
            let (a, b) = (c.f[fa as usize], c.f[fb as usize]);
            let (l, r) = match ins.op {
                PsMerge00 => (a.ps0, b.ps0),
                PsMerge01 => (a.ps0, b.ps1),
                PsMerge10 => (a.ps1, b.ps0),
                _ => (a.ps1, b.ps1),
            };
            set(c, fd, l, r);
            rc1(c, ins);
        }
        PsMr | PsNeg | PsAbs | PsNabs | PsRes | PsRsqrte => {
            let (x, y) = (f0(c, fb), f1(c, fb));
            let (nx, ny) = match ins.op {
                PsMr => (x, y),
                PsNeg => (-x, -y),
                PsAbs => (x.abs(), y.abs()),
                PsNabs => (-x.abs(), -y.abs()),
                PsRes => (fepor_fres(x), fepor_fres(y)),
                _ => (fepor_frsqrte(x), fepor_frsqrte(y)),
            };
            set(c, fd, nx, ny);
            rc1(c, ins);
        }
        PsSel => {
            let x = fsel(f0(c, fa), f0(c, fb), f0(c, fc));
            let y = fsel(f1(c, fa), f1(c, fb), f1(c, fc));
            set(c, fd, x, y);
            rc1(c, ins);
        }
        PsCmpu0 | PsCmpo0 | PsCmpu1 | PsCmpo1 => {
            let hi = matches!(ins.op, PsCmpu1 | PsCmpo1);
            let (a, b) = if hi {
                (f1(c, fa), f1(c, fb))
            } else {
                (f0(c, fa), f0(c, fb))
            };
            set_cr(c, ins.field_crfd(), fcmp(a, b));
        }
        // ---- FPSCR ----
        Mffs => {
            c.f[fd as usize].ps0 = f64::from_bits(0xFFF8_0000_0000_0000 | c.fpscr as u64);
            rc1(c, ins);
        }
        Mtfsf => {
            let fm = ins.field_mtfsf_fm();
            let mut m = 0u32;
            for i in 0..8 {
                if fm & (0x80 >> i) != 0 {
                    m |= 0xF << (28 - 4 * i);
                }
            }
            c.fpscr = (c.fpscr & !m) | (f0(c, fb).to_bits() as u32 & m);
            rc1(c, ins);
        }
        Mtfsfi => {
            let n = ins.field_crfd() as u32;
            let m = 0xFu32 << (28 - 4 * n);
            c.fpscr = (c.fpscr & !m) | ((ins.field_mtfsf_imm() as u32) << (28 - 4 * n));
            rc1(c, ins);
        }
        Mtfsb0 | Mtfsb1 => {
            let m = 0x8000_0000u32 >> ins.field_crbd();
            if ins.op == Mtfsb0 {
                c.fpscr &= !m;
            } else {
                c.fpscr |= m;
            }
            rc1(c, ins);
        }
        Mcrfs => {
            let n = ins.field_crfs() as u32;
            let v = c.fpscr >> (28 - 4 * n) & 0xF;
            set_cr(
                c,
                ins.field_crfd(),
                [
                    (v >> 3 & 1) as u8,
                    (v >> 2 & 1) as u8,
                    (v >> 1 & 1) as u8,
                    (v & 1) as u8,
                ],
            );
        }
        other => panic!("fepor-jit: {other:?} is not an FP instruction"),
    }
}

/// Floating-point and quantized loads and stores whose effective address the JIT already computed.
///
/// # Safety
/// `c` must be a live guest context.
pub unsafe extern "C" fn fepor_jit_fpmem(c: *mut Context, word: u32, ea: u32) {
    use Opcode::*;
    let c = &mut *c;
    let ins = decode(word);
    let fd = ins.field_frd();
    match ins.op {
        Lfs | Lfsu | Lfsx | Lfsux => {
            let v = single_to_double(ld32(c, ea));
            set_both(c, fd, v);
        }
        Lfd | Lfdu | Lfdx | Lfdux => {
            let v = f64::from_bits(ld64(c, ea));
            c.f[fd as usize].ps0 = v;
        }
        Stfs | Stfsu | Stfsx | Stfsux => st32(c, ea, double_to_single(f0(c, ins.field_frs()))),
        Stfd | Stfdu | Stfdx | Stfdux => st64(c, ea, f0(c, ins.field_frs()).to_bits()),
        Stfiwx => st32(c, ea, f0(c, ins.field_frs()).to_bits() as u32),
        PsqL | PsqLu | PsqLx | PsqLux => {
            let (i, w) = if matches!(ins.op, PsqL | PsqLu) {
                (ins.field_ps_i() as usize, ins.field_ps_w() != 0)
            } else {
                (ins.field_ps_ix() as usize, ins.field_ps_wx() != 0)
            };
            let (a, b) = psq_ld(c, ea, c.spr[912 + i], w);
            set(c, fd, a, b);
        }
        PsqSt | PsqStu | PsqStx | PsqStux => {
            let (i, w) = if matches!(ins.op, PsqSt | PsqStu) {
                (ins.field_ps_i() as usize, ins.field_ps_w() != 0)
            } else {
                (ins.field_ps_ix() as usize, ins.field_ps_wx() != 0)
            };
            let s = ins.field_frs();
            psq_st(c, ea, c.spr[912 + i], w, (f0(c, s), f1(c, s)));
        }
        other => panic!("fepor-jit: {other:?} is not an FP memory instruction"),
    }
}

/// `dcbz`: zero the 32-byte line holding `ea`.
///
/// # Safety
/// `c` must be a live guest context.
pub unsafe extern "C" fn fepor_jit_dcbz(c: *mut Context, ea: u32) {
    let c = &*c;
    let ea = ea & !31;
    for i in (0..32).step_by(4) {
        st32(c, ea + i, 0);
    }
}

/// `tw`/`twi` condition (TO bits: 16 lt, 8 gt, 4 eq, 2 ltu, 1 gtu).
pub extern "C" fn fepor_jit_tw(to: u32, a: u32, b: u32) -> u32 {
    let (sa, sb) = (a as i32, b as i32);
    ((sa < sb && to & 16 != 0)
        || (sa > sb && to & 8 != 0)
        || (sa == sb && to & 4 != 0)
        || (a < b && to & 2 != 0)
        || (a > b && to & 1 != 0)) as u32
}
