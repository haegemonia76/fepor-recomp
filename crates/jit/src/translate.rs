//! One guest function to Cranelift IR and native code.
//!
//! The model is the C++ backend's: the integer registers, CR bits, LR, CTR and XER live in SSA variables
//! inside a function and are stored back to the `Context` around calls and on return (`out`) and reloaded
//! after calls (`inn`); floating-point registers always live in the context and FP instructions run in
//! out-of-line helpers (`fp.rs`). Memory is big-endian guest RAM at `ctx.ram`; addresses outside RAM go to
//! the runtime's hardware register handlers.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, FuncRef, InstBuilder, MemFlagsData, Signature, Type, Value,
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use fepor_core::analysis::Function;
use ppc750cl::{Ins, Opcode};

use crate::{fp, HleSymbols, Jit, State};

const I32: Type = types::I32;
const I8: Type = types::I8;
const I64: Type = types::I64;

// Offsets in `fepor_runtime::Context` (layout asserted in crates/runtime/src/context.rs).
const OFF_R: i32 = 0;
const OFF_CR: i32 = 640;
const OFF_LR: i32 = 672;
const OFF_CTR: i32 = 676;
const OFF_XER: i32 = 680;
const OFF_MSR: i32 = 688;
const OFF_SPR: i32 = 692;
const OFF_SR: i32 = 4788;
const OFF_RESERVE: i32 = 4852;
const OFF_RAM: i32 = 4856;
/// `spr[1023]`: what OSLoadContext tells its caller (1 = this thread was resumed from an interrupt).
const SPR_HLE_STATUS: i32 = 1023;
/// `spr[1022]`: set while the host frames of an interrupted thread are being abandoned (see `Jit::entry`).
pub(crate) const SPR_UNWIND: i32 = 1022;
/// `spr[1021]`: the guest address a resume entry (`Jit::resume_entry`) continues at.
const SPR_RESUME_PC: i32 = 1021;
/// `spr[1020]`: the guest address the last poll site continues at (an interrupt delivered there resumes it).
const SPR_POLL_PC: i32 = 1020;

const RAM_SIZE: i64 = 0x0180_0000;

// ---- imported runtime functions -------------------------------------------------------------------------

use fepor_runtime::abi::{
    fepor_enter, fepor_hw_read, fepor_hw_write, fepor_lc_dma, fepor_mfdec, fepor_mftb, fepor_mtdec,
    fepor_poll, fepor_rfi, fepor_syscall, fepor_trap, fepor_unimplemented, fepor_unresolved,
};

extern "C" fn jit_resolve(jit: *const Jit, index: usize) -> *mut u8 {
    // SAFETY: the pointer was embedded by `compile` from a `&'static Jit`.
    unsafe { &*jit }.resolve(index)
}

extern "C" fn jit_lookup(jit: *const Jit, from: u32, target: u32) -> *mut u8 {
    // SAFETY: as above.
    let jit = unsafe { &*jit };
    match jit.index.get(&target) {
        Some(&i) => jit.resolve(i),
        None => fepor_runtime::abi::die_str(&format!(
            "indirect branch at {from:08x} to {target:08x}: not a function"
        )),
    }
}

pub(crate) fn register_symbols(jb: &mut JITBuilder, hle: &HleSymbols) {
    let syms: &[(&str, *const u8)] = &[
        ("fepor_hw_read", fepor_hw_read as *const u8),
        ("fepor_hw_write", fepor_hw_write as *const u8),
        ("fepor_mftb", fepor_mftb as *const u8),
        ("fepor_mfdec", fepor_mfdec as *const u8),
        ("fepor_mtdec", fepor_mtdec as *const u8),
        ("fepor_lc_dma", fepor_lc_dma as *const u8),
        ("fepor_enter", fepor_enter as *const u8),
        ("fepor_poll", fepor_poll as *const u8),
        ("fepor_rfi", fepor_rfi as *const u8),
        ("fepor_syscall", fepor_syscall as *const u8),
        ("fepor_trap", fepor_trap as *const u8),
        ("fepor_unimplemented", fepor_unimplemented as *const u8),
        ("fepor_unresolved", fepor_unresolved as *const u8),
        ("jit_resolve", jit_resolve as *const u8),
        ("jit_lookup", jit_lookup as *const u8),
        ("fepor_jit_fp", fp::fepor_jit_fp as *const u8),
        ("fepor_jit_fpmem", fp::fepor_jit_fpmem as *const u8),
        ("fepor_jit_dcbz", fp::fepor_jit_dcbz as *const u8),
        ("fepor_jit_tw", fp::fepor_jit_tw as *const u8),
        ("fepor_jit_s2d", fp::fepor_jit_s2d as *const u8),
        ("fepor_jit_d2s", fp::fepor_jit_d2s as *const u8),
    ];
    for &(n, p) in syms {
        jb.symbol(n, p);
    }
    for (n, a) in hle {
        jb.symbol(format!("fepor_hle_{n}"), *a as *const u8);
    }
}

// ---- state of one translation ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Used {
    gpr: u32,
    cr: bool,
    lr: bool,
    ctr: bool,
    xer: bool,
}

struct Tr<'a, 'm> {
    jit: &'a Jit,
    f: &'a Function,
    b: FunctionBuilder<'m>,
    module: &'m mut JITModule,
    imports: &'m mut HashMap<String, FuncId>,
    refs: HashMap<String, FuncRef>,
    ctx: Value,
    ram: Value,
    ptr: Type,
    gpr: [Variable; 32],
    cr: [Variable; 32],
    lr: Variable,
    ctr: Variable,
    xso: Variable,
    xov: Variable,
    xca: Variable,
    xbc: Variable,
    /// What this pass touched.
    used: Used,
    /// What the previous pass touched: the registers loaded on entry and stored by `out`. `None` in the
    /// first pass, whose output is thrown away.
    fixed: Option<Used>,
    blocks: BTreeMap<u32, Block>,
    resume_labels: BTreeMap<u32, u32>,
    /// Compile the resume variant (see `Jit::resume_entry`): it enters at the address in `spr[1021]`.
    resume: bool,
    terminated: bool,
}

fn flags() -> MemFlagsData {
    MemFlagsData::trusted()
}

/// A signature in the host's C calling convention (System V, Windows or Apple): guest functions are called
/// from Rust and call Rust.
fn sig(conv: CallConv, params: &[Type], ret: Option<Type>) -> Signature {
    let mut s = Signature::new(conv);
    s.params.extend(params.iter().map(|&t| AbiParam::new(t)));
    if let Some(r) = ret {
        s.returns.push(AbiParam::new(r));
    }
    s
}

fn mask(mb: u8, me: u8) -> u32 {
    let (mb, me) = (mb as u32, me as u32);
    let m = (u32::MAX >> mb) & (u32::MAX << (31 - me));
    if mb <= me {
        m
    } else {
        !((u32::MAX >> (me + 1)) & (u32::MAX << (32 - mb)))
    }
}

pub(crate) fn compile(jit: &Jit, st: &mut State, i: usize, resume: bool) -> *mut u8 {
    let f = &jit.functions[i];
    let State {
        module,
        clif,
        fbc,
        imports,
        counter,
        ..
    } = st;
    let ptr = module.target_config().pointer_type();
    let mut s = Signature::new(module.isa().default_call_conv());
    s.params.push(AbiParam::new(ptr));
    *counter += 1;
    let name = format!("{}_{:08x}_{}", if resume { "r" } else { "f" }, f.start, counter);
    let id = module
        .declare_function(&name, Linkage::Local, &s)
        .expect("declare function");

    // pass 1: which registers does the function touch? (the output is discarded)
    let used = {
        module.clear_context(clif);
        clif.func.signature = s.clone();
        let mut t = Tr::new(jit, f, &mut clif.func, fbc, module, imports, ptr, None);
        t.resume = resume;
        t.run()
    };
    module.clear_context(clif);
    clif.func.signature = s;
    {
        let mut t = Tr::new(
            jit,
            f,
            &mut clif.func,
            fbc,
            module,
            imports,
            ptr,
            Some(used),
        );
        t.resume = resume;
        t.run();
    }
    if std::env::var_os("FEPOR_JIT_DUMP")
        .is_some_and(|v| v.to_string_lossy() == format!("{:08x}", f.start))
    {
        eprintln!("{}", clif.func.display());
    }
    module
        .define_function(id, clif)
        .unwrap_or_else(|e| panic!("fepor-jit: compiling {:08x}: {e:?}", f.start));
    module.clear_context(clif);
    module.finalize_definitions().expect("finalize");
    module.get_finalized_function(id) as *mut u8
}

impl<'a, 'm> Tr<'a, 'm> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        jit: &'a Jit,
        f: &'a Function,
        func: &'m mut cranelift_codegen::ir::Function,
        fbc: &'m mut cranelift_frontend::FunctionBuilderContext,
        module: &'m mut JITModule,
        imports: &'m mut HashMap<String, FuncId>,
        ptr: Type,
        fixed: Option<Used>,
    ) -> Tr<'a, 'm> {
        let mut b = FunctionBuilder::new(func, fbc);
        let gpr = std::array::from_fn(|_| b.declare_var(I32));
        let cr = std::array::from_fn(|_| b.declare_var(I8));
        let (lr, ctr) = (b.declare_var(I32), b.declare_var(I32));
        let (xso, xov, xca, xbc) = (
            b.declare_var(I8),
            b.declare_var(I8),
            b.declare_var(I8),
            b.declare_var(I8),
        );
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let ctx = b.block_params(entry)[0];
        let ram = b.ins().load(ptr, flags(), ctx, OFF_RAM);
        Tr {
            jit,
            f,
            b,
            module,
            imports,
            refs: HashMap::new(),
            ctx,
            ram,
            ptr,
            gpr,
            cr,
            lr,
            ctr,
            xso,
            xov,
            xca,
            xbc,
            used: Used::default(),
            fixed,
            blocks: BTreeMap::new(),
            resume_labels: BTreeMap::new(),
            resume: false,
            terminated: false,
        }
    }

    // ---- plumbing ----

    fn import(&mut self, name: &str, params: &[Type], ret: Option<Type>) -> FuncRef {
        if let Some(&r) = self.refs.get(name) {
            return r;
        }
        let id = match self.imports.get(name) {
            Some(&id) => id,
            None => {
                let id = self
                    .module
                    .declare_function(
                        name,
                        Linkage::Import,
                        &sig(self.module.isa().default_call_conv(), params, ret),
                    )
                    .expect("declare import");
                self.imports.insert(name.to_string(), id);
                id
            }
        };
        let r = self.module.declare_func_in_func(id, self.b.func);
        self.refs.insert(name.to_string(), r);
        r
    }

    fn call(
        &mut self,
        name: &str,
        params: &[Type],
        ret: Option<Type>,
        args: &[Value],
    ) -> Option<Value> {
        let r = self.import(name, params, ret);
        let inst = self.b.ins().call(r, args);
        self.b.inst_results(inst).first().copied()
    }

    fn c32(&mut self, v: u32) -> Value {
        self.b.ins().iconst(I32, v as i64)
    }

    fn jitptr(&mut self) -> Value {
        self.b.ins().iconst(self.ptr, self.jit as *const Jit as i64)
    }

    // ---- registers ----

    fn r(&mut self, n: u8) -> Value {
        self.used.gpr |= 1 << n;
        self.b.use_var(self.gpr[n as usize])
    }
    fn set_r(&mut self, n: u8, v: Value) {
        self.used.gpr |= 1 << n;
        self.b.def_var(self.gpr[n as usize], v);
    }
    fn ra0(&mut self, n: u8) -> Value {
        if n == 0 {
            self.c32(0)
        } else {
            self.r(n)
        }
    }
    fn crbit(&mut self, bit: u8) -> Value {
        self.used.cr = true;
        self.b.use_var(self.cr[bit as usize])
    }
    fn set_crbit(&mut self, bit: u8, v: Value) {
        self.used.cr = true;
        self.b.def_var(self.cr[bit as usize], v);
    }
    fn so(&mut self) -> Value {
        self.used.xer = true;
        self.b.use_var(self.xso)
    }
    fn lr_v(&mut self) -> Value {
        self.used.lr = true;
        self.b.use_var(self.lr)
    }
    fn ctr_v(&mut self) -> Value {
        self.used.ctr = true;
        self.b.use_var(self.ctr)
    }

    /// Store the registers the function touched back to the context.
    fn out(&mut self) {
        let Some(u) = self.fixed else { return };
        for n in 0..32 {
            if u.gpr >> n & 1 != 0 {
                let v = self.b.use_var(self.gpr[n]);
                self.b
                    .ins()
                    .store(flags(), v, self.ctx, OFF_R + 4 * n as i32);
            }
        }
        if u.cr {
            for k in 0..32 {
                let v = self.b.use_var(self.cr[k]);
                self.b.ins().store(flags(), v, self.ctx, OFF_CR + k as i32);
            }
        }
        if u.lr {
            let v = self.b.use_var(self.lr);
            self.b.ins().store(flags(), v, self.ctx, OFF_LR);
        }
        if u.ctr {
            let v = self.b.use_var(self.ctr);
            self.b.ins().store(flags(), v, self.ctx, OFF_CTR);
        }
        if u.xer {
            for (k, var) in [self.xso, self.xov, self.xca, self.xbc]
                .into_iter()
                .enumerate()
            {
                let v = self.b.use_var(var);
                self.b.ins().store(flags(), v, self.ctx, OFF_XER + k as i32);
            }
        }
    }

    /// Reload them (after a call that may have changed the context).
    fn inn(&mut self) {
        let u = match self.fixed {
            Some(u) => u,
            // first pass: define every variable once so that later uses are well formed
            None => Used {
                gpr: u32::MAX,
                cr: true,
                lr: true,
                ctr: true,
                xer: true,
            },
        };
        for n in 0..32 {
            if u.gpr >> n & 1 != 0 {
                let v = self
                    .b
                    .ins()
                    .load(I32, flags(), self.ctx, OFF_R + 4 * n as i32);
                self.b.def_var(self.gpr[n], v);
            }
        }
        if u.cr {
            for k in 0..32 {
                let v = self.b.ins().load(I8, flags(), self.ctx, OFF_CR + k as i32);
                self.b.def_var(self.cr[k], v);
            }
        }
        if u.lr {
            let v = self.b.ins().load(I32, flags(), self.ctx, OFF_LR);
            self.b.def_var(self.lr, v);
        }
        if u.ctr {
            let v = self.b.ins().load(I32, flags(), self.ctx, OFF_CTR);
            self.b.def_var(self.ctr, v);
        }
        if u.xer {
            for (k, var) in [self.xso, self.xov, self.xca, self.xbc]
                .into_iter()
                .enumerate()
            {
                let v = self.b.ins().load(I8, flags(), self.ctx, OFF_XER + k as i32);
                self.b.def_var(var, v);
            }
        }
    }

    /// Reload one CR field a helper wrote into the context.
    fn reload_cr_field(&mut self, n: u8) {
        self.used.cr = true;
        for k in 0..4 {
            let v = self
                .b
                .ins()
                .load(I8, flags(), self.ctx, OFF_CR + 4 * n as i32 + k);
            self.b.def_var(self.cr[4 * n as usize + k as usize], v);
        }
    }

    // ---- memory ----

    /// Run `fast(host_address)` for an address in RAM, `slow(ea)` otherwise; the result of either is merged.
    fn mem_access(
        &mut self,
        ea: Value,
        ty: Option<Type>,
        fast: impl FnOnce(&mut Self, Value) -> Option<Value>,
        slow: impl FnOnce(&mut Self, Value) -> Option<Value>,
    ) -> Option<Value> {
        let a = self.b.ins().band_imm_s(ea, 0x3FFF_FFFF);
        let ok = self
            .b
            .ins()
            .icmp_imm_s(IntCC::UnsignedLessThan, a, RAM_SIZE);
        let (fb, sb, mb) = (
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
        );
        if let Some(t) = ty {
            self.b.append_block_param(mb, t);
        }
        self.b.set_cold_block(sb);
        self.b.ins().brif(ok, fb, &[], sb, &[]);
        self.b.switch_to_block(fb);
        let a64 = self.b.ins().uextend(I64, a);
        let host = self.b.ins().iadd(self.ram, a64);
        let v = fast(self, host);
        match v {
            Some(v) => self.b.ins().jump(mb, &[v.into()]),
            None => self.b.ins().jump(mb, &[]),
        };
        self.b.switch_to_block(sb);
        let v = slow(self, ea);
        match v {
            Some(v) => self.b.ins().jump(mb, &[v.into()]),
            None => self.b.ins().jump(mb, &[]),
        };
        self.b.switch_to_block(mb);
        ty.map(|_| self.b.block_params(mb)[0])
    }

    /// Load of 1, 2 or 4 bytes, zero-extended to 32 bits.
    fn ld(&mut self, ea: Value, size: u8) -> Value {
        let ty = match size {
            1 => I8,
            2 => types::I16,
            _ => I32,
        };
        self.mem_access(
            ea,
            Some(I32),
            |s, host| {
                let v = s.b.ins().load(ty, flags(), host, 0);
                let v = match size {
                    1 => v,
                    _ => s.b.ins().bswap(v),
                };
                Some(if size == 4 {
                    v
                } else {
                    s.b.ins().uextend(I32, v)
                })
            },
            |s, ea| {
                let sz = s.b.ins().iconst(I32, size as i64);
                let v = s.call("fepor_hw_read", &[I32, I32], Some(I32), &[ea, sz])?;
                // a byte or halfword load takes only the low bits of what the register returned
                Some(match size {
                    1 => s.b.ins().band_imm_s(v, 0xFF),
                    2 => s.b.ins().band_imm_s(v, 0xFFFF),
                    _ => v,
                })
            },
        )
        .expect("load value")
    }

    fn st(&mut self, ea: Value, v: Value, size: u8) {
        self.mem_access(
            ea,
            None,
            |s, host| {
                match size {
                    1 => {
                        let b8 = s.b.ins().ireduce(I8, v);
                        s.b.ins().store(flags(), b8, host, 0);
                    }
                    2 => {
                        let h = s.b.ins().ireduce(types::I16, v);
                        let h = s.b.ins().bswap(h);
                        s.b.ins().store(flags(), h, host, 0);
                    }
                    _ => {
                        let w = s.b.ins().bswap(v);
                        s.b.ins().store(flags(), w, host, 0);
                    }
                }
                None
            },
            |s, ea| {
                let sz = s.b.ins().iconst(I32, size as i64);
                // a byte or halfword store writes only the low bits of the register
                let v = match size {
                    1 => s.b.ins().band_imm_s(v, 0xFF),
                    2 => s.b.ins().band_imm_s(v, 0xFFFF),
                    _ => v,
                };
                s.call("fepor_hw_write", &[I32, I32, I32], None, &[ea, v, sz]);
                None
            },
        );
    }

    fn ea_d(&mut self, a: u8, d: i16) -> Value {
        let base = self.ra0(a);
        if d == 0 {
            base
        } else {
            self.b.ins().iadd_imm_s(base, d as i64)
        }
    }
    fn ea_x(&mut self, a: u8, b: u8) -> Value {
        let rb = self.r(b);
        if a == 0 {
            rb
        } else {
            let ra = self.r(a);
            self.b.ins().iadd(ra, rb)
        }
    }

    // ---- condition register ----

    fn set_cr_field(&mut self, n: u8, lt: Value, gt: Value, eq: Value, so: Value) {
        let k = 4 * n;
        self.set_crbit(k, lt);
        self.set_crbit(k + 1, gt);
        self.set_crbit(k + 2, eq);
        self.set_crbit(k + 3, so);
    }

    fn cmp_field(&mut self, n: u8, a: Value, b: Value, signed: bool) {
        let (lt_cc, gt_cc) = if signed {
            (IntCC::SignedLessThan, IntCC::SignedGreaterThan)
        } else {
            (IntCC::UnsignedLessThan, IntCC::UnsignedGreaterThan)
        };
        let lt = self.b.ins().icmp(lt_cc, a, b);
        let gt = self.b.ins().icmp(gt_cc, a, b);
        let eq = self.b.ins().icmp(IntCC::Equal, a, b);
        let so = self.so();
        self.set_cr_field(n, lt, gt, eq, so);
    }

    /// CR0 after a record-form instruction.
    fn rc0(&mut self, ins: Ins, v: Value) {
        if ins.field_rc() {
            let z = self.c32(0);
            self.cmp_field(0, v, z, true);
        }
    }

    fn set_ov(&mut self, ov: Value) {
        self.used.xer = true;
        self.b.def_var(self.xov, ov);
        let so = self.b.use_var(self.xso);
        let so = self.b.ins().bor(so, ov);
        self.b.def_var(self.xso, so);
    }

    fn b8(&mut self, v: Value) -> Value {
        // a 0/1 value in an i32 as an i8
        self.b.ins().ireduce(I8, v)
    }

    // ---- control flow ----

    fn block_at(&mut self, addr: u32) -> Block {
        *self
            .blocks
            .entry(addr)
            .or_insert_with(|| self.b.create_block())
    }

    fn terminate_with_return(&mut self) {
        self.b.ins().return_(&[]);
        self.terminated = true;
    }

    /// Call guest function `target` (a known function start).
    fn call_function(&mut self, target: u32) {
        let i = self.jit.index[&target];
        let slot = self.jit.table.as_ptr() as i64 + 8 * i as i64;
        let slotv = self.b.ins().iconst(self.ptr, slot);
        let p = self.b.ins().load(self.ptr, flags(), slotv, 0);
        let missing = self.b.ins().icmp_imm_s(IntCC::Equal, p, 0);
        let (slow, go) = (self.b.create_block(), self.b.create_block());
        self.b.append_block_param(go, self.ptr);
        self.b.set_cold_block(slow);
        self.b.ins().brif(missing, slow, &[], go, &[p.into()]);
        self.b.switch_to_block(slow);
        let jp = self.jitptr();
        let idx = self.b.ins().iconst(self.ptr, i as i64);
        let r = self
            .call(
                "jit_resolve",
                &[self.ptr, self.ptr],
                Some(self.ptr),
                &[jp, idx],
            )
            .expect("resolved");
        self.b.ins().jump(go, &[r.into()]);
        self.b.switch_to_block(go);
        let callee = self.b.block_params(go)[0];
        let s = self.b.import_signature(sig(
            self.module.isa().default_call_conv(),
            &[self.ptr],
            None,
        ));
        self.b.ins().call_indirect(s, callee, &[self.ctx]);
    }

    /// Call whatever function `target` (a runtime value) names.
    fn call_indirect_guest(&mut self, from: u32, target: Value) {
        let jp = self.jitptr();
        let fromv = self.c32(from);
        let p = self
            .call(
                "jit_lookup",
                &[self.ptr, I32, I32],
                Some(self.ptr),
                &[jp, fromv, target],
            )
            .expect("looked up");
        let s = self.b.import_signature(sig(
            self.module.isa().default_call_conv(),
            &[self.ptr],
            None,
        ));
        self.b.ins().call_indirect(s, p, &[self.ctx]);
    }

    /// After a call: return at once when the interrupted thread's host frames are being abandoned.
    fn check_unwind(&mut self) {
        if self.fixed.is_none() {
            return;
        }
        let flag = self
            .b
            .ins()
            .load(I32, flags(), self.ctx, OFF_SPR + 4 * SPR_UNWIND);
        let (leave, go) = (self.b.create_block(), self.b.create_block());
        self.b.set_cold_block(leave);
        self.b.ins().brif(flag, leave, &[], go, &[]);
        self.b.switch_to_block(leave);
        self.b.ins().return_(&[]);
        self.b.switch_to_block(go);
    }

    fn unresolved(&mut self, from: u32, target: Value) {
        self.out();
        let fromv = self.c32(from);
        self.call(
            "fepor_unresolved",
            &[self.ptr, I32, I32],
            None,
            &[self.ctx, fromv, target],
        );
        self.terminate_with_return();
    }

    /// Branch (not a call) to guest address `t` from `from`.
    fn jump(&mut self, from: u32, t: u32) {
        let internal = t >= self.f.start && t < self.f.end;
        if internal {
            if t <= from {
                // a loop needs a safe point too: a guest spinning on a flag an interrupt sets never calls anything
                self.tick(t);
            }
            let blk = self.block_at(t);
            self.b.ins().jump(blk, &[]);
            self.terminated = true;
        } else if self.jit.index.contains_key(&t) {
            self.out();
            self.call_function(t);
            self.terminate_with_return();
        } else {
            let tv = self.c32(t);
            self.unresolved(from, tv);
        }
    }

    /// Safe point of a loop that continues at guest address `resume`.
    fn tick(&mut self, resume: u32) {
        let addr = std::ptr::addr_of_mut!(fepor_runtime::abi::fepor_ticks) as i64;
        let a = self.b.ins().iconst(self.ptr, addr);
        let v = self.b.ins().load(I32, flags(), a, 0);
        let v = self.b.ins().iadd_imm_s(v, -1);
        self.b.ins().store(flags(), v, a, 0);
        let (poll, go) = (self.b.create_block(), self.b.create_block());
        self.b.set_cold_block(poll);
        self.b.ins().brif(v, go, &[], poll, &[]);
        self.b.switch_to_block(poll);
        self.out();
        // a save state taken here, or an interrupt delivered here that switches threads, resumes the loop at `resume`
        let at = self.c32(resume);
        self.b
            .ins()
            .store(flags(), at, self.ctx, OFF_SPR + 4 * SPR_POLL_PC);
        self.call("fepor_poll", &[self.ptr], None, &[self.ctx]);
        // a thread whose frames a state load abandons returns from here like from a call
        self.check_unwind();
        self.inn();
        self.b.ins().jump(go, &[]);
        self.b.switch_to_block(go);
    }

    /// The branch condition for BO/BI (also decrements CTR); `None` means always.
    fn cond(&mut self, bo: u8, bi: u8) -> Option<Value> {
        let mut conds: Vec<Value> = Vec::new();
        if bo & 4 == 0 {
            let ctr = self.ctr_v();
            let ctr = self.b.ins().iadd_imm_s(ctr, -1);
            self.b.def_var(self.ctr, ctr);
            let cc = if bo & 2 == 0 {
                IntCC::NotEqual
            } else {
                IntCC::Equal
            };
            conds.push(self.b.ins().icmp_imm_s(cc, ctr, 0));
        }
        if bo & 0x10 == 0 {
            let bit = self.crbit(bi);
            conds.push(if bo & 8 != 0 {
                bit
            } else {
                self.b.ins().bxor_imm_s(bit, 1)
            });
        }
        let mut it = conds.into_iter();
        let first = it.next()?;
        Some(it.fold(first, |a, c| self.b.ins().band(a, c)))
    }

    /// Run `then` when `cond` holds (always when `None`); control continues after it.
    fn guarded(&mut self, cond: Option<Value>, then: impl FnOnce(&mut Self)) {
        match cond {
            None => then(self),
            Some(c) => {
                let (t, rest) = (self.b.create_block(), self.b.create_block());
                self.b.ins().brif(c, t, &[], rest, &[]);
                self.b.switch_to_block(t);
                then(self);
                if !self.terminated {
                    self.b.ins().jump(rest, &[]);
                }
                self.terminated = false;
                self.b.switch_to_block(rest);
            }
        }
    }

    fn branch(&mut self, addr: u32, ins: Ins) {
        let ret = addr + 4;
        let lk = ins.field_lk();
        let (bo, bi) = (ins.field_bo(), ins.field_bi());
        match ins.op {
            Opcode::B => {
                let t = ins.branch_dest(addr).unwrap_or(0);
                if lk {
                    self.call_known(addr, t);
                } else {
                    self.jump(addr, t);
                }
            }
            Opcode::Bc => {
                let t = ins.branch_dest(addr).unwrap_or(0);
                let c = self.cond(bo, bi);
                if lk {
                    let r = self.c32(ret);
                    self.used.lr = true;
                    self.b.def_var(self.lr, r);
                    if t != ret {
                        self.guarded(c, |s| s.call_known_after_lr(addr, t));
                    }
                } else {
                    self.guarded(c, |s| s.jump(addr, t));
                }
            }
            Opcode::Bclr => {
                let target = self.lr_v();
                if lk {
                    let r = self.c32(ret);
                    self.b.def_var(self.lr, r);
                    let c = self.cond(bo, bi);
                    self.guarded(c, |s| {
                        s.out();
                        s.call_indirect_guest(addr, target);
                        s.check_unwind();
                        s.inn();
                    });
                } else {
                    let c = self.cond(bo, bi);
                    self.guarded(c, |s| {
                        s.out();
                        s.terminate_with_return();
                    });
                }
            }
            Opcode::Bcctr => {
                let target = self.ctr_v();
                if lk {
                    let r = self.c32(ret);
                    self.used.lr = true;
                    self.b.def_var(self.lr, r);
                    let c = self.cond(bo, bi);
                    self.guarded(c, |s| {
                        s.out();
                        s.call_indirect_guest(addr, target);
                        s.check_unwind();
                        s.inn();
                    });
                } else {
                    let c = self.cond(bo, bi);
                    let table = self.jit.tables.get(&addr).cloned();
                    self.guarded(c, |s| match table {
                        Some(t) => s.switch_jump(addr, target, &t.targets),
                        None => {
                            s.out();
                            s.call_indirect_guest(addr, target);
                            s.terminate_with_return();
                        }
                    });
                }
            }
            _ => unreachable!(),
        }
    }

    fn switch_jump(&mut self, addr: u32, target: Value, targets: &[u32]) {
        let mut sw = Switch::new();
        let mut cases: BTreeMap<u32, Block> = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for &t in targets {
            if seen.insert(t) {
                let blk = self.b.create_block();
                cases.insert(t, blk);
                sw.set_entry(t as u128, blk);
            }
        }
        let default = self.b.create_block();
        sw.emit(&mut self.b, target, default);
        for (t, blk) in cases {
            self.b.switch_to_block(blk);
            self.jump(addr, t);
        }
        self.b.switch_to_block(default);
        self.unresolved(addr, target);
    }

    /// `bl target`: LR is set; call the function.
    fn call_known(&mut self, addr: u32, t: u32) {
        let r = self.c32(addr + 4);
        self.used.lr = true;
        self.b.def_var(self.lr, r);
        self.call_known_after_lr(addr, t);
    }

    fn call_known_after_lr(&mut self, from: u32, t: u32) {
        if !self.jit.index.contains_key(&t) {
            let tv = self.c32(t);
            self.unresolved(from, tv);
            return;
        }
        self.out();
        self.call_function(t);
        self.check_unwind();
        self.inn();
        if let Some(h) = self.jit.hle.get(&t) {
            if h.unwinds && self.fixed.is_some() {
                // OSLoadContext resumed this thread from an interrupt: abandon the host frames
                let st = self
                    .b
                    .ins()
                    .load(I32, flags(), self.ctx, OFF_SPR + 4 * SPR_HLE_STATUS);
                let one = self.b.ins().icmp_imm_s(IntCC::Equal, st, 1);
                let (leave, go) = (self.b.create_block(), self.b.create_block());
                self.b.set_cold_block(leave);
                self.b.ins().brif(one, leave, &[], go, &[]);
                self.b.switch_to_block(leave);
                let one32 = self.c32(1);
                self.b
                    .ins()
                    .store(flags(), one32, self.ctx, OFF_SPR + 4 * SPR_UNWIND);
                self.b.ins().return_(&[]);
                self.b.switch_to_block(go);
            }
            if let Some(label) = h
                .resumes_at
                .and_then(|save| self.resume_labels.get(&save))
                .copied()
            {
                let blk = self.block_at(label);
                self.b.ins().jump(blk, &[]);
                self.terminated = true;
            }
        }
    }

    // ---- the instructions ----

    fn unimplemented(&mut self, addr: u32, ins: Ins) {
        self.out();
        let (a, w) = (self.c32(addr), self.c32(ins.code));
        self.call(
            "fepor_unimplemented",
            &[self.ptr, I32, I32],
            None,
            &[self.ctx, a, w],
        );
        self.terminate_with_return();
    }

    fn fp_exec(&mut self, ins: Ins) {
        if self.inline_fp(ins) {
            return;
        }
        self.fp_helper(ins);
    }

    fn fp_helper(&mut self, ins: Ins) {
        let w = self.c32(ins.code);
        self.call("fepor_jit_fp", &[self.ptr, I32], None, &[self.ctx, w]);
        if let Some(n) = fp::cr_field_written(ins) {
            self.reload_cr_field(n);
        }
    }

    fn fpr(&mut self, n: u8, lane: i32) -> Value {
        self.b.ins().load(
            types::F64,
            flags(),
            self.ctx,
            128 + 16 * n as i32 + 8 * lane,
        )
    }

    fn set_fpr(&mut self, n: u8, lane: i32, v: Value) {
        self.b
            .ins()
            .store(flags(), v, self.ctx, 128 + 16 * n as i32 + 8 * lane);
    }

    /// `round_single`: to single precision and back.
    fn round_single(&mut self, v: Value) -> Value {
        let s = self.b.ins().fdemote(types::F32, v);
        self.b.ins().fpromote(types::F64, s)
    }

    /// The multiplier of a single-precision multiply keeps its top 25 mantissa bits (infinities and NaNs pass).
    fn force25(&mut self, v: Value) -> Value {
        let b = self.b.ins().bitcast(I64, MemFlagsData::new(), v);
        let exp = self.b.ins().ushr_imm_s(b, 52);
        let exp = self.b.ins().band_imm_s(exp, 0x7FF);
        let special = self.b.ins().icmp_imm_s(IntCC::Equal, exp, 0x7FF);
        let hi = self.b.ins().band_imm_s(b, 0xFFFF_FFFF_F800_0000u64 as i64);
        let lo = self.b.ins().band_imm_s(b, 0x0800_0000);
        let forced = self.b.ins().iadd(hi, lo);
        let r = self.b.ins().select(special, b, forced);
        self.b.ins().bitcast(types::F64, MemFlagsData::new(), r)
    }

    /// Instructions whose result is the plain IEEE one unless it is a NaN (the PowerPC rules for which NaN
    /// comes out are in the helper): compute, and when any result is a NaN run the helper instead.
    /// Returns false for the ones left to the helper (record forms, estimates, selects, compares, FPSCR).
    fn inline_fp(&mut self, ins: Ins) -> bool {
        use Opcode::*;
        if ins.field_rc() {
            return false;
        }
        let (fd, fa, fb, fc) = (
            ins.field_frd(),
            ins.field_fra(),
            ins.field_frb(),
            ins.field_frc(),
        );
        let single = matches!(
            ins.op,
            Fadds | Fsubs | Fmuls | Fdivs | Fmadds | Fmsubs | Fnmadds | Fnmsubs
        );
        // moves and sign changes: no arithmetic, no NaN rules
        match ins.op {
            Fmr | Fneg | Fabs | Fnabs => {
                let y = self.fpr(fb, 0);
                let v = match ins.op {
                    Fmr => y,
                    Fneg => self.b.ins().fneg(y),
                    Fabs => self.b.ins().fabs(y),
                    _ => {
                        let a = self.b.ins().fabs(y);
                        self.b.ins().fneg(a)
                    }
                };
                self.set_fpr(fd, 0, v);
                return true;
            }
            PsMr | PsNeg | PsAbs | PsNabs => {
                for lane in 0..2 {
                    let y = self.fpr(fb, lane);
                    let v = match ins.op {
                        PsMr => y,
                        PsNeg => self.b.ins().fneg(y),
                        PsAbs => self.b.ins().fabs(y),
                        _ => {
                            let a = self.b.ins().fabs(y);
                            self.b.ins().fneg(a)
                        }
                    };
                    self.set_fpr(fd, lane, v);
                }
                return true;
            }
            PsMerge00 | PsMerge01 | PsMerge10 | PsMerge11 => {
                let (la, lb) = match ins.op {
                    PsMerge00 => (0, 0),
                    PsMerge01 => (0, 1),
                    PsMerge10 => (1, 0),
                    _ => (1, 1),
                };
                let (x, y) = (self.fpr(fa, la), self.fpr(fb, lb));
                self.set_fpr(fd, 0, x);
                self.set_fpr(fd, 1, y);
                return true;
            }
            _ => {}
        }
        // lanes computed: (result lane 0, result lane 1 for the paired ones)
        let paired = matches!(
            ins.op,
            PsAdd
                | PsSub
                | PsMul
                | PsDiv
                | PsMadd
                | PsMsub
                | PsNmadd
                | PsNmsub
                | PsMuls0
                | PsMuls1
                | PsMadds0
                | PsMadds1
        );
        let scalar = matches!(
            ins.op,
            Fadd | Fsub
                | Fmul
                | Fdiv
                | Fadds
                | Fsubs
                | Fmuls
                | Fdivs
                | Fmadd
                | Fmsub
                | Fnmadd
                | Fnmsub
                | Fmadds
                | Fmsubs
                | Fnmadds
                | Fnmsubs
        );
        if !paired && !scalar {
            return false;
        }
        let lanes = if paired { 2 } else { 1 };
        let mut results = Vec::new();
        for lane in 0..lanes {
            let (a, b) = (self.fpr(fa, lane), self.fpr(fb, lane));
            // the multiplier: the register C itself, or one lane of it for the `s0`/`s1` forms
            let cl = match ins.op {
                PsMuls0 | PsMadds0 => 0,
                PsMuls1 | PsMadds1 => 1,
                _ => lane,
            };
            let mut c = self.fpr(fc, cl);
            if single || paired {
                c = self.force25(c);
            }
            let mut r = match ins.op {
                Fadd | Fadds | PsAdd => self.b.ins().fadd(a, b),
                Fsub | Fsubs | PsSub => self.b.ins().fsub(a, b),
                Fdiv | Fdivs | PsDiv => self.b.ins().fdiv(a, b),
                Fmul | Fmuls | PsMul | PsMuls0 | PsMuls1 => {
                    let m = if matches!(ins.op, Fmul) {
                        self.fpr(fc, lane)
                    } else {
                        c
                    };
                    self.b.ins().fmul(a, m)
                }
                Fmadd | Fmadds | Fnmadd | Fnmadds | PsMadd | PsNmadd | PsMadds0 | PsMadds1 => {
                    let m = if matches!(ins.op, Fmadd | Fnmadd) {
                        self.fpr(fc, lane)
                    } else {
                        c
                    };
                    self.b.ins().fma(a, m, b)
                }
                _ => {
                    // msub, nmsub: a * c - b
                    let m = if matches!(ins.op, Fmsub | Fnmsub) {
                        self.fpr(fc, lane)
                    } else {
                        c
                    };
                    let nb = self.b.ins().fneg(b);
                    self.b.ins().fma(a, m, nb)
                }
            };
            if single || paired {
                r = self.round_single(r);
            }
            if matches!(
                ins.op,
                Fnmadd | Fnmadds | Fnmsub | Fnmsubs | PsNmadd | PsNmsub
            ) {
                // negated unless it is a NaN, which goes to the helper anyway
                r = self.b.ins().fneg(r);
            }
            results.push(r);
        }
        // a NaN anywhere: the PowerPC rules pick which one, the helper knows them
        let mut bad = self
            .b
            .ins()
            .fcmp(FloatCC::Unordered, results[0], results[0]);
        if lanes == 2 {
            let b1 = self
                .b
                .ins()
                .fcmp(FloatCC::Unordered, results[1], results[1]);
            bad = self.b.ins().bor(bad, b1);
        }
        let (slow, store, done) = (
            self.b.create_block(),
            self.b.create_block(),
            self.b.create_block(),
        );
        self.b.set_cold_block(slow);
        self.b.ins().brif(bad, slow, &[], store, &[]);
        self.b.switch_to_block(slow);
        self.fp_helper(ins);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(store);
        if paired {
            self.set_fpr(fd, 0, results[0]);
            self.set_fpr(fd, 1, results[1]);
        } else if single {
            // single-precision scalars fill both halves
            self.set_fpr(fd, 0, results[0]);
            self.set_fpr(fd, 1, results[0]);
        } else {
            self.set_fpr(fd, 0, results[0]);
        }
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        true
    }

    fn fp_mem(&mut self, ins: Ins, ea: Value, update: Option<u8>) {
        use Opcode::*;
        match ins.op {
            Lfs | Lfsu | Lfsx | Lfsux => {
                let bits = self.ld(ea, 4);
                // an infinity or NaN keeps its payload bits (the helper), everything else is a plain widening
                let exp = self.b.ins().ushr_imm_s(bits, 23);
                let exp = self.b.ins().band_imm_s(exp, 0xFF);
                let special = self.b.ins().icmp_imm_s(IntCC::Equal, exp, 0xFF);
                let (slow, fast, merge) = (
                    self.b.create_block(),
                    self.b.create_block(),
                    self.b.create_block(),
                );
                self.b.append_block_param(merge, types::F64);
                self.b.set_cold_block(slow);
                self.b.ins().brif(special, slow, &[], fast, &[]);
                self.b.switch_to_block(fast);
                let s = self.b.ins().bitcast(types::F32, MemFlagsData::new(), bits);
                let v = self.b.ins().fpromote(types::F64, s);
                self.b.ins().jump(merge, &[v.into()]);
                self.b.switch_to_block(slow);
                let r = self
                    .call("fepor_jit_s2d", &[I32], Some(I64), &[bits])
                    .expect("s2d result");
                let v = self.b.ins().bitcast(types::F64, MemFlagsData::new(), r);
                self.b.ins().jump(merge, &[v.into()]);
                self.b.switch_to_block(merge);
                let v = self.b.block_params(merge)[0];
                let fd = ins.field_frd();
                self.set_fpr(fd, 0, v);
                self.set_fpr(fd, 1, v);
            }
            Lfd | Lfdu | Lfdx | Lfdux => {
                let hi = self.ld(ea, 4);
                let ea4 = self.b.ins().iadd_imm_s(ea, 4);
                let lo = self.ld(ea4, 4);
                let hi = self.b.ins().uextend(I64, hi);
                let lo = self.b.ins().uextend(I64, lo);
                let hi = self.b.ins().ishl_imm_s(hi, 32);
                let bits = self.b.ins().bor(hi, lo);
                let v = self.b.ins().bitcast(types::F64, MemFlagsData::new(), bits);
                self.set_fpr(ins.field_frd(), 0, v);
            }
            Stfs | Stfsu | Stfsx | Stfsux => {
                let d = self.fpr(ins.field_frs(), 0);
                let x = self.b.ins().bitcast(I64, MemFlagsData::new(), d);
                // normal range, zero: the single is the top bits (truncated, as the hardware does); the
                // small values that become denormal singles go to the helper
                let exp = self.b.ins().ushr_imm_s(x, 52);
                let exp = self.b.ins().band_imm_s(exp, 0x7FF);
                let big = self
                    .b
                    .ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThan, exp, 896);
                let twice = self.b.ins().ishl_imm_s(x, 1);
                let zero = self.b.ins().icmp_imm_s(IntCC::Equal, twice, 0);
                let plain = self.b.ins().bor(big, zero);
                let (slow, fast, merge) = (
                    self.b.create_block(),
                    self.b.create_block(),
                    self.b.create_block(),
                );
                self.b.append_block_param(merge, I32);
                self.b.set_cold_block(slow);
                self.b.ins().brif(plain, fast, &[], slow, &[]);
                self.b.switch_to_block(fast);
                let top = self.b.ins().ushr_imm_s(x, 32);
                let top = self.b.ins().ireduce(I32, top);
                let sign_exp = self.b.ins().band_imm_s(top, 0xC000_0000u32 as i64);
                let low = self.b.ins().ushr_imm_s(x, 29);
                let low = self.b.ins().ireduce(I32, low);
                let low = self.b.ins().band_imm_s(low, 0x3FFF_FFFF);
                let v = self.b.ins().bor(sign_exp, low);
                self.b.ins().jump(merge, &[v.into()]);
                self.b.switch_to_block(slow);
                let v = self
                    .call("fepor_jit_d2s", &[I64], Some(I32), &[x])
                    .expect("d2s result");
                self.b.ins().jump(merge, &[v.into()]);
                self.b.switch_to_block(merge);
                let v = self.b.block_params(merge)[0];
                self.st(ea, v, 4);
            }
            Stfd | Stfdu | Stfdx | Stfdux => {
                let d = self.fpr(ins.field_frs(), 0);
                let x = self.b.ins().bitcast(I64, MemFlagsData::new(), d);
                let hi = self.b.ins().ushr_imm_s(x, 32);
                let hi = self.b.ins().ireduce(I32, hi);
                let lo = self.b.ins().ireduce(I32, x);
                self.st(ea, hi, 4);
                let ea4 = self.b.ins().iadd_imm_s(ea, 4);
                self.st(ea4, lo, 4);
            }
            _ => {
                let w = self.c32(ins.code);
                self.call(
                    "fepor_jit_fpmem",
                    &[self.ptr, I32, I32],
                    None,
                    &[self.ctx, w, ea],
                );
            }
        }
        if let Some(a) = update {
            self.set_r(a, ea);
        }
    }

    fn load_gpr(&mut self, d: u8, v: Value, upd: Option<(u8, Value)>) {
        self.set_r(d, v);
        if let Some((a, ea)) = upd {
            self.set_r(a, ea);
        }
    }

    fn xer_pack(&mut self) -> Value {
        self.used.xer = true;
        let (so, ov, ca, bc) = (
            self.b.use_var(self.xso),
            self.b.use_var(self.xov),
            self.b.use_var(self.xca),
            self.b.use_var(self.xbc),
        );
        let w = |s: &mut Self, v: Value, sh: i64| {
            let v = s.b.ins().uextend(I32, v);
            s.b.ins().ishl_imm_s(v, sh)
        };
        let (a, b, c) = (w(self, so, 31), w(self, ov, 30), w(self, ca, 29));
        let bc = self.b.ins().uextend(I32, bc);
        let bc = self.b.ins().band_imm_s(bc, 0x7F);
        let x = self.b.ins().bor(a, b);
        let x = self.b.ins().bor(x, c);
        self.b.ins().bor(x, bc)
    }

    fn xer_unpack(&mut self, v: Value) {
        self.used.xer = true;
        for (var, sh) in [(self.xso, 31), (self.xov, 30), (self.xca, 29)] {
            let b = self.b.ins().ushr_imm_s(v, sh);
            let b = self.b.ins().band_imm_s(b, 1);
            let b = self.b.ins().ireduce(I8, b);
            self.b.def_var(var, b);
        }
        let bc = self.b.ins().band_imm_s(v, 0x7F);
        let bc = self.b.ins().ireduce(I8, bc);
        self.b.def_var(self.xbc, bc);
    }

    fn adde(&mut self, a: Value, b: Value, carry_in: Value) -> Value {
        // carry_in: an i8 0/1
        self.used.xer = true;
        let (a64, b64) = (self.b.ins().uextend(I64, a), self.b.ins().uextend(I64, b));
        let c64 = self.b.ins().uextend(I64, carry_in);
        let t = self.b.ins().iadd(a64, b64);
        let t = self.b.ins().iadd(t, c64);
        let ca = self.b.ins().ushr_imm_s(t, 32);
        let ca = self.b.ins().ireduce(I8, ca);
        self.b.def_var(self.xca, ca);
        self.b.ins().ireduce(I32, t)
    }

    pub fn lift(&mut self, addr: u32, ins: Ins) {
        use Opcode::*;
        let d = ins.field_rd();
        let a = ins.field_ra();
        let b = ins.field_rb();
        let s = ins.field_rs();
        let sh = ins.field_sh();
        let oe = ins.field_oe();
        let uimm = ins.field_uimm();
        let simmv = ins.field_simm();
        let off = ins.field_offset();

        match ins.op {
            // ---- integer arithmetic ----
            Addi => {
                let ra = self.ra0(a);
                let v = if a == 0 {
                    self.c32(simmv as i32 as u32)
                } else {
                    self.b.ins().iadd_imm_s(ra, simmv as i64)
                };
                self.set_r(d, v);
            }
            Addis => {
                let ra = self.ra0(a);
                let imm = (uimm as u32) << 16;
                let v = if a == 0 {
                    self.c32(imm)
                } else {
                    self.b.ins().iadd_imm_s(ra, imm as i32 as i64)
                };
                self.set_r(d, v);
            }
            Addic | Addic_ => {
                self.used.xer = true;
                let ra = self.r(a);
                let imm = self.c32(simmv as i32 as u32);
                let zero = self.b.ins().iconst(I8, 0);
                let v = self.adde(ra, imm, zero);
                self.set_r(d, v);
                if ins.op == Addic_ {
                    let z = self.c32(0);
                    self.cmp_field(0, v, z, true);
                }
            }
            Subfic => {
                let ra = self.r(a);
                let na = self.b.ins().bnot(ra);
                let imm = self.c32(simmv as i32 as u32);
                let one = self.b.ins().iconst(I8, 1);
                let v = self.adde(na, imm, one);
                self.set_r(d, v);
            }
            Mulli => {
                let ra = self.r(a);
                let v = self.b.ins().imul_imm_s(ra, simmv as i64);
                self.set_r(d, v);
            }
            Add | Subf | Neg | Mullw | Divw | Divwu | Mulhw | Mulhwu | Addc | Adde | Addze
            | Addme | Subfc | Subfe | Subfze | Subfme => {
                let carry_op = matches!(
                    ins.op,
                    Addc | Adde | Addze | Addme | Subfc | Subfe | Subfze | Subfme
                );
                if oe && (carry_op || matches!(ins.op, Mulhw | Mulhwu)) {
                    return self.unimplemented(addr, ins);
                }
                if carry_op || oe {
                    self.used.xer = true;
                }
                let ra = self.r(a);
                let rb = if matches!(ins.op, Neg | Addze | Addme | Subfze | Subfme) {
                    ra
                } else {
                    self.r(b)
                };
                let v = match ins.op {
                    Add => {
                        let r = self.b.ins().iadd(ra, rb);
                        if oe {
                            let x = self.b.ins().bxor(ra, rb);
                            let nx = self.b.ins().bnot(x);
                            let y = self.b.ins().bxor(ra, r);
                            let o = self.b.ins().band(nx, y);
                            let o = self.b.ins().ushr_imm_s(o, 31);
                            let o = self.b8(o);
                            self.set_ov(o);
                        }
                        r
                    }
                    Subf => {
                        let r = self.b.ins().isub(rb, ra);
                        if oe {
                            let x = self.b.ins().bxor(ra, rb);
                            let y = self.b.ins().bxor(rb, r);
                            let o = self.b.ins().band(x, y);
                            let o = self.b.ins().ushr_imm_s(o, 31);
                            let o = self.b8(o);
                            self.set_ov(o);
                        }
                        r
                    }
                    Neg => {
                        if oe {
                            let o = self.b.ins().icmp_imm_s(
                                IntCC::Equal,
                                ra,
                                0x8000_0000u32 as i32 as i64,
                            );
                            self.set_ov(o);
                        }
                        self.b.ins().ineg(ra)
                    }
                    Mullw => {
                        let r = self.b.ins().imul(ra, rb);
                        if oe {
                            let (x, y) =
                                (self.b.ins().sextend(I64, ra), self.b.ins().sextend(I64, rb));
                            let p = self.b.ins().imul(x, y);
                            let lo = self.b.ins().ireduce(I32, p);
                            let back = self.b.ins().sextend(I64, lo);
                            let o = self.b.ins().icmp(IntCC::NotEqual, p, back);
                            self.set_ov(o);
                        }
                        r
                    }
                    Divw => {
                        let min =
                            self.b
                                .ins()
                                .icmp_imm_s(IntCC::Equal, ra, 0x8000_0000u32 as i32 as i64);
                        let m1 = self.b.ins().icmp_imm_s(IntCC::Equal, rb, -1);
                        let zero = self.b.ins().icmp_imm_s(IntCC::Equal, rb, 0);
                        let ovf = self.b.ins().band(min, m1);
                        let bad = self.b.ins().bor(ovf, zero);
                        let one = self.c32(1);
                        let den = self.b.ins().select(bad, one, rb);
                        let q = self.b.ins().sdiv(ra, den);
                        let neg = self.b.ins().sshr_imm_s(ra, 31); // all ones when negative, else 0
                        let r = self.b.ins().select(bad, neg, q);
                        if oe {
                            self.set_ov(bad);
                        }
                        r
                    }
                    Divwu => {
                        let zero = self.b.ins().icmp_imm_s(IntCC::Equal, rb, 0);
                        let one = self.c32(1);
                        let den = self.b.ins().select(zero, one, rb);
                        let q = self.b.ins().udiv(ra, den);
                        let z = self.c32(0);
                        let r = self.b.ins().select(zero, z, q);
                        if oe {
                            self.set_ov(zero);
                        }
                        r
                    }
                    Mulhw => self.b.ins().smulhi(ra, rb),
                    Mulhwu => self.b.ins().umulhi(ra, rb),
                    Addc => {
                        let zero = self.b.ins().iconst(I8, 0);
                        self.adde(ra, rb, zero)
                    }
                    Adde => {
                        let ca = self.b.use_var(self.xca);
                        self.adde(ra, rb, ca)
                    }
                    Addze | Addme => {
                        let ca = self.b.use_var(self.xca);
                        let k = self.c32(if ins.op == Addze { 0 } else { u32::MAX });
                        self.adde(ra, k, ca)
                    }
                    Subfc => {
                        let na = self.b.ins().bnot(ra);
                        let one = self.b.ins().iconst(I8, 1);
                        self.adde(na, rb, one)
                    }
                    Subfe => {
                        let na = self.b.ins().bnot(ra);
                        let ca = self.b.use_var(self.xca);
                        self.adde(na, rb, ca)
                    }
                    _ => {
                        // subfze, subfme
                        let na = self.b.ins().bnot(ra);
                        let ca = self.b.use_var(self.xca);
                        let k = self.c32(if ins.op == Subfze { 0 } else { u32::MAX });
                        self.adde(na, k, ca)
                    }
                };
                self.set_r(d, v);
                self.rc0(ins, v);
            }
            // ---- logical ----
            And | Andc | Or | Orc | Xor | Nand | Nor | Eqv => {
                let (rs, rb) = (self.r(s), self.r(b));
                let v = match ins.op {
                    And => self.b.ins().band(rs, rb),
                    Andc => self.b.ins().band_not(rs, rb),
                    Or => self.b.ins().bor(rs, rb),
                    Orc => self.b.ins().bor_not(rs, rb),
                    Xor => self.b.ins().bxor(rs, rb),
                    Nand => {
                        let x = self.b.ins().band(rs, rb);
                        self.b.ins().bnot(x)
                    }
                    Nor => {
                        let x = self.b.ins().bor(rs, rb);
                        self.b.ins().bnot(x)
                    }
                    _ => {
                        let x = self.b.ins().bxor(rs, rb);
                        self.b.ins().bnot(x)
                    }
                };
                self.set_r(a, v);
                self.rc0(ins, v);
            }
            Ori | Oris | Xori | Xoris | Andi_ | Andis_ => {
                if ins.op == Ori && uimm == 0 && a == 0 && s == 0 {
                    return; // nop
                }
                let imm = match ins.op {
                    Oris | Xoris | Andis_ => (uimm as u32) << 16,
                    _ => uimm as u32,
                } as i32 as i64;
                let rs = self.r(s);
                let v = match ins.op {
                    Ori | Oris => self.b.ins().bor_imm_s(rs, imm),
                    Xori | Xoris => self.b.ins().bxor_imm_s(rs, imm),
                    _ => self.b.ins().band_imm_s(rs, imm),
                };
                self.set_r(a, v);
                if matches!(ins.op, Andi_ | Andis_) {
                    let z = self.c32(0);
                    self.cmp_field(0, v, z, true);
                }
            }
            Extsb | Extsh | Cntlzw => {
                let rs = self.r(s);
                let v = match ins.op {
                    Extsb => {
                        let x = self.b.ins().ireduce(I8, rs);
                        self.b.ins().sextend(I32, x)
                    }
                    Extsh => {
                        let x = self.b.ins().ireduce(types::I16, rs);
                        self.b.ins().sextend(I32, x)
                    }
                    _ => self.b.ins().clz(rs),
                };
                self.set_r(a, v);
                self.rc0(ins, v);
            }
            Slw | Srw => {
                let (rs, rb) = (self.r(s), self.r(b));
                let sh = self.b.ins().band_imm_s(rb, 31);
                let shifted = if ins.op == Slw {
                    self.b.ins().ishl(rs, sh)
                } else {
                    self.b.ins().ushr(rs, sh)
                };
                let big = self.b.ins().band_imm_s(rb, 0x20);
                let z = self.c32(0);
                let v = self.b.ins().select(big, z, shifted);
                self.set_r(a, v);
                self.rc0(ins, v);
            }
            Sraw | Srawi => {
                self.used.xer = true;
                let rs = self.r(s);
                let amt = if ins.op == Sraw {
                    let rb = self.r(b);
                    self.b.ins().band_imm_s(rb, 0x3F)
                } else {
                    self.c32(sh as u32)
                };
                let big = self.b.ins().band_imm_s(amt, 0x20);
                let n = self.b.ins().band_imm_s(amt, 31);
                let shifted = self.b.ins().sshr(rs, n);
                let sign = self.b.ins().ushr_imm_s(rs, 31);
                let one = self.c32(1);
                let bit = self.b.ins().ishl(one, n);
                let lowmask = self.b.ins().iadd_imm_s(bit, -1);
                let lost = self.b.ins().band(rs, lowmask);
                let lost = self.b.ins().icmp_imm_s(IntCC::NotEqual, lost, 0);
                let lost = self.b.ins().uextend(I32, lost);
                let nz = self.b.ins().icmp_imm_s(IntCC::NotEqual, n, 0);
                let nz = self.b.ins().uextend(I32, nz);
                let ca_small = self.b.ins().band(sign, lost);
                let ca_small = self.b.ins().band(ca_small, nz);
                let fill = self.b.ins().sshr_imm_s(rs, 31);
                let v = self.b.ins().select(big, fill, shifted);
                let ca = self.b.ins().select(big, sign, ca_small);
                let ca = self.b8(ca);
                self.b.def_var(self.xca, ca);
                self.set_r(a, v);
                self.rc0(ins, v);
            }
            Rlwinm | Rlwimi | Rlwnm => {
                let m = mask(ins.field_mb(), ins.field_me());
                let rs = self.r(s);
                let rot = if ins.op == Rlwnm {
                    let rb = self.r(b);
                    self.b.ins().rotl(rs, rb)
                } else if sh == 0 {
                    rs
                } else {
                    self.b.ins().rotl_imm_s(rs, sh as i64)
                };
                let v = if ins.op == Rlwimi {
                    let ra = self.r(a);
                    let x = self.b.ins().band_imm_s(rot, m as i32 as i64);
                    let y = self.b.ins().band_imm_s(ra, !m as i32 as i64);
                    self.b.ins().bor(x, y)
                } else if m == u32::MAX {
                    rot
                } else {
                    self.b.ins().band_imm_s(rot, m as i32 as i64)
                };
                self.set_r(a, v);
                self.rc0(ins, v);
            }
            // ---- compare ----
            Cmp | Cmpl | Cmpi | Cmpli => {
                let ra = self.r(a);
                let rhs = match ins.op {
                    Cmp | Cmpl => self.r(b),
                    Cmpi => self.c32(simmv as i32 as u32),
                    _ => self.c32(uimm as u32),
                };
                self.cmp_field(ins.field_crfd(), ra, rhs, matches!(ins.op, Cmp | Cmpi));
            }
            // ---- integer loads ----
            Lbz | Lbzu | Lhz | Lhzu | Lha | Lhau | Lwz | Lwzu => {
                let upd = matches!(ins.op, Lbzu | Lhzu | Lhau | Lwzu);
                let ea = self.ea_d(a, off);
                let size = match ins.op {
                    Lbz | Lbzu => 1,
                    Lhz | Lhzu | Lha | Lhau => 2,
                    _ => 4,
                };
                let mut v = self.ld(ea, size);
                if matches!(ins.op, Lha | Lhau) {
                    let h = self.b.ins().ireduce(types::I16, v);
                    v = self.b.ins().sextend(I32, h);
                }
                self.load_gpr(d, v, upd.then_some((a, ea)));
            }
            Lbzx | Lbzux | Lhzx | Lhzux | Lhax | Lhaux | Lwzx | Lwzux | Lhbrx | Lwbrx => {
                let upd = matches!(ins.op, Lbzux | Lhzux | Lhaux | Lwzux);
                let ea = self.ea_x(a, b);
                let size = match ins.op {
                    Lbzx | Lbzux => 1,
                    Lhzx | Lhzux | Lhax | Lhaux | Lhbrx => 2,
                    _ => 4,
                };
                let mut v = self.ld(ea, size);
                match ins.op {
                    Lhax | Lhaux => {
                        let h = self.b.ins().ireduce(types::I16, v);
                        v = self.b.ins().sextend(I32, h);
                    }
                    Lhbrx => {
                        let h = self.b.ins().ireduce(types::I16, v);
                        let h = self.b.ins().bswap(h);
                        v = self.b.ins().uextend(I32, h);
                    }
                    Lwbrx => v = self.b.ins().bswap(v),
                    _ => {}
                }
                self.load_gpr(d, v, upd.then_some((a, ea)));
            }
            Lwarx => {
                let ea = self.ea_x(a, b);
                let v = self.ld(ea, 4);
                self.set_r(d, v);
                self.b.ins().store(flags(), ea, self.ctx, OFF_RESERVE);
            }
            Lmw => {
                let mut ea = self.ea_d(a, off);
                for k in d..32 {
                    let v = self.ld(ea, 4);
                    self.set_r(k, v);
                    ea = self.b.ins().iadd_imm_s(ea, 4);
                }
            }
            Stmw => {
                let mut ea = self.ea_d(a, off);
                for k in s..32 {
                    let v = self.r(k);
                    self.st(ea, v, 4);
                    ea = self.b.ins().iadd_imm_s(ea, 4);
                }
            }
            // ---- integer stores ----
            Stb | Stbu | Sth | Sthu | Stw | Stwu => {
                let upd = matches!(ins.op, Stbu | Sthu | Stwu);
                let ea = self.ea_d(a, off);
                let rs = self.r(s);
                let size = match ins.op {
                    Stb | Stbu => 1,
                    Sth | Sthu => 2,
                    _ => 4,
                };
                self.st(ea, rs, size);
                if upd {
                    self.set_r(a, ea);
                }
            }
            Stbx | Stbux | Sthx | Sthux | Stwx | Stwux | Sthbrx | Stwbrx => {
                let upd = matches!(ins.op, Stbux | Sthux | Stwux);
                let ea = self.ea_x(a, b);
                let mut rs = self.r(s);
                let size = match ins.op {
                    Stbx | Stbux => 1,
                    Sthx | Sthux | Sthbrx => 2,
                    _ => 4,
                };
                match ins.op {
                    Sthbrx => {
                        let h = self.b.ins().ireduce(types::I16, rs);
                        let h = self.b.ins().bswap(h);
                        rs = self.b.ins().uextend(I32, h);
                    }
                    Stwbrx => rs = self.b.ins().bswap(rs),
                    _ => {}
                }
                self.st(ea, rs, size);
                if upd {
                    self.set_r(a, ea);
                }
            }
            Stwcx_ => {
                let ea = self.ea_x(a, b);
                let rs = self.r(s);
                self.st(ea, rs, 4);
                let (z, o) = (self.b.ins().iconst(I8, 0), self.b.ins().iconst(I8, 1));
                let so = self.so();
                self.set_cr_field(0, z, z, o, so);
            }
            // ---- FP loads/stores ----
            Lfs | Lfsu | Lfd | Lfdu | Stfs | Stfsu | Stfd | Stfdu => {
                let upd = matches!(ins.op, Lfsu | Lfdu | Stfsu | Stfdu);
                let ea = self.ea_d(a, off);
                self.fp_mem(ins, ea, upd.then_some(a));
            }
            Lfsx | Lfsux | Lfdx | Lfdux | Stfsx | Stfsux | Stfdx | Stfdux | Stfiwx => {
                let upd = matches!(ins.op, Lfsux | Lfdux | Stfsux | Stfdux);
                let ea = self.ea_x(a, b);
                self.fp_mem(ins, ea, upd.then_some(a));
            }
            PsqL | PsqLu | PsqSt | PsqStu => {
                let ea = self.ea_d(a, ins.field_ps_offset());
                self.fp_mem(ins, ea, matches!(ins.op, PsqLu | PsqStu).then_some(a));
            }
            PsqLx | PsqLux | PsqStx | PsqStux => {
                let ea = self.ea_x(a, b);
                self.fp_mem(ins, ea, matches!(ins.op, PsqLux | PsqStux).then_some(a));
            }
            // ---- branches ----
            B | Bc | Bclr | Bcctr => self.branch(addr, ins),
            // ---- condition register ----
            Crand | Crandc | Creqv | Crnand | Crnor | Cror | Crorc | Crxor => {
                let (x, y) = (self.crbit(ins.field_crba()), self.crbit(ins.field_crbb()));
                let v = match ins.op {
                    Crand => self.b.ins().band(x, y),
                    Crandc => {
                        let ny = self.b.ins().bxor_imm_s(y, 1);
                        self.b.ins().band(x, ny)
                    }
                    Creqv => {
                        let t = self.b.ins().bxor(x, y);
                        self.b.ins().bxor_imm_s(t, 1)
                    }
                    Crnand => {
                        let t = self.b.ins().band(x, y);
                        self.b.ins().bxor_imm_s(t, 1)
                    }
                    Crnor => {
                        let t = self.b.ins().bor(x, y);
                        self.b.ins().bxor_imm_s(t, 1)
                    }
                    Cror => self.b.ins().bor(x, y),
                    Crorc => {
                        let ny = self.b.ins().bxor_imm_s(y, 1);
                        self.b.ins().bor(x, ny)
                    }
                    _ => self.b.ins().bxor(x, y),
                };
                self.set_crbit(ins.field_crbd(), v);
            }
            Mcrf => {
                let (dst, src) = (ins.field_crfd(), ins.field_crfs());
                for k in 0..4 {
                    let v = self.crbit(4 * src + k);
                    self.set_crbit(4 * dst + k, v);
                }
            }
            Mcrxr => {
                self.used.xer = true;
                let dst = ins.field_crfd();
                let (so, ov, ca) = (
                    self.b.use_var(self.xso),
                    self.b.use_var(self.xov),
                    self.b.use_var(self.xca),
                );
                let z = self.b.ins().iconst(I8, 0);
                self.set_cr_field(dst, so, ov, ca, z);
                for var in [self.xso, self.xov, self.xca] {
                    self.b.def_var(var, z);
                }
            }
            Mfcr => {
                let mut v = self.c32(0);
                for k in 0..32 {
                    let bit = self.crbit(k);
                    let bit = self.b.ins().uextend(I32, bit);
                    let bit = self.b.ins().ishl_imm_s(bit, 31 - k as i64);
                    v = self.b.ins().bor(v, bit);
                }
                self.set_r(d, v);
            }
            Mtcrf => {
                let rs = self.r(s);
                let crm = ins.field_crm() as u32;
                for i in 0..8u8 {
                    if crm & (0x80 >> i) != 0 {
                        for k in 0..4u8 {
                            let bit = self.b.ins().ushr_imm_s(rs, 31 - (4 * i + k) as i64);
                            let bit = self.b.ins().band_imm_s(bit, 1);
                            let bit = self.b.ins().ireduce(I8, bit);
                            self.set_crbit(4 * i + k, bit);
                        }
                    }
                }
            }
            // ---- special registers ----
            Mfspr => {
                let v = match ins.field_spr() {
                    1 => self.xer_pack(),
                    8 => self.lr_v(),
                    9 => self.ctr_v(),
                    22 => self
                        .call("fepor_mfdec", &[], Some(I32), &[])
                        .expect("mfdec"),
                    n => self
                        .b
                        .ins()
                        .load(I32, flags(), self.ctx, OFF_SPR + 4 * n as i32),
                };
                self.set_r(d, v);
            }
            Mtspr => {
                let rs = self.r(s);
                match ins.field_spr() {
                    1 => self.xer_unpack(rs),
                    8 => {
                        self.used.lr = true;
                        self.b.def_var(self.lr, rs);
                    }
                    9 => {
                        self.used.ctr = true;
                        self.b.def_var(self.ctr, rs);
                    }
                    22 => {
                        self.call("fepor_mtdec", &[I32], None, &[rs]);
                    }
                    923 => {
                        self.b.ins().store(flags(), rs, self.ctx, OFF_SPR + 4 * 923);
                        self.call("fepor_lc_dma", &[self.ptr], None, &[self.ctx]);
                    }
                    n => {
                        self.b
                            .ins()
                            .store(flags(), rs, self.ctx, OFF_SPR + 4 * n as i32);
                    }
                }
            }
            Mftb => {
                let tbr = self.c32(ins.field_tbr() as u32);
                let v = self
                    .call("fepor_mftb", &[I32], Some(I32), &[tbr])
                    .expect("mftb");
                self.set_r(d, v);
            }
            Mfmsr => {
                let v = self.b.ins().load(I32, flags(), self.ctx, OFF_MSR);
                self.set_r(d, v);
            }
            Mtmsr => {
                let rs = self.r(s);
                self.b.ins().store(flags(), rs, self.ctx, OFF_MSR);
            }
            Mfsr => {
                let v =
                    self.b
                        .ins()
                        .load(I32, flags(), self.ctx, OFF_SR + 4 * ins.field_sr() as i32);
                self.set_r(d, v);
            }
            Mtsr => {
                let rs = self.r(s);
                self.b
                    .ins()
                    .store(flags(), rs, self.ctx, OFF_SR + 4 * ins.field_sr() as i32);
            }
            Mfsrin | Mtsrin => {
                let rb = self.r(b);
                let idx = self.b.ins().ushr_imm_s(rb, 28);
                let idx = self.b.ins().uextend(I64, idx);
                let idx = self.b.ins().ishl_imm_s(idx, 2);
                let base = self.b.ins().iadd(self.ctx, idx);
                if ins.op == Mfsrin {
                    let v = self.b.ins().load(I32, flags(), base, OFF_SR);
                    self.set_r(d, v);
                } else {
                    let rs = self.r(s);
                    self.b.ins().store(flags(), rs, base, OFF_SR);
                }
            }
            // ---- FPSCR and floating point ----
            Mffs | Mtfsf | Mtfsfi | Mtfsb0 | Mtfsb1 | Mcrfs | Fmr | Fabs | Fneg | Fnabs | Fadd
            | Fsub | Fmul | Fdiv | Fadds | Fsubs | Fmuls | Fdivs | Fmadd | Fmsub | Fnmadd
            | Fnmsub | Fmadds | Fmsubs | Fnmadds | Fnmsubs | Fres | Frsqrte | Frsp | Fsel
            | Fctiw | Fctiwz | Fcmpu | Fcmpo | PsAdd | PsSub | PsMul | PsDiv | PsMadd | PsMsub
            | PsNmadd | PsNmsub | PsMuls0 | PsMuls1 | PsMadds0 | PsMadds1 | PsSum0 | PsSum1
            | PsMerge00 | PsMerge01 | PsMerge10 | PsMerge11 | PsMr | PsNeg | PsAbs | PsNabs
            | PsRes | PsRsqrte | PsSel | PsCmpu0 | PsCmpo0 | PsCmpu1 | PsCmpo1 => self.fp_exec(ins),
            // ---- cache, sync, system ----
            Sync | Isync | Eieio | Dcbf | Dcbi | Dcbst | Dcbt | Dcbtst | Icbi | Tlbie | Tlbsync => {
            }
            Dcbz | DcbzL => {
                let ea = self.ea_x(a, b);
                self.call("fepor_jit_dcbz", &[self.ptr, I32], None, &[self.ctx, ea]);
            }
            Twi | Tw => {
                let to = ins.field_to();
                if to != 0 {
                    let ra = self.r(a);
                    let rhs = if ins.op == Twi {
                        self.c32(simmv as i32 as u32)
                    } else {
                        self.r(b)
                    };
                    let tov = self.c32(to as u32);
                    let hit = self
                        .call("fepor_jit_tw", &[I32, I32, I32], Some(I32), &[tov, ra, rhs])
                        .expect("tw");
                    self.guarded(Some(hit), |s| {
                        s.out();
                        let av = s.c32(addr);
                        s.call("fepor_trap", &[s.ptr, I32], None, &[s.ctx, av]);
                        s.terminate_with_return();
                    });
                }
            }
            Sc => {
                self.out();
                let av = self.c32(addr);
                self.call("fepor_syscall", &[self.ptr, I32], None, &[self.ctx, av]);
                self.inn();
            }
            Rfi => {
                self.out();
                let av = self.c32(addr);
                self.call("fepor_rfi", &[self.ptr, I32], None, &[self.ctx, av]);
                self.terminate_with_return();
            }
            _ => self.unimplemented(addr, ins),
        }
    }

    // ---- driver ----

    /// Make the next instruction's block current: a label's block, or a fresh one after a terminator.
    fn begin(&mut self, addr: u32) {
        if let Some(&blk) = self.blocks.get(&addr) {
            if !self.terminated {
                self.b.ins().jump(blk, &[]);
            }
            self.b.switch_to_block(blk);
            self.terminated = false;
        } else if self.terminated {
            let dead = self.b.create_block();
            self.b.switch_to_block(dead);
            self.terminated = false;
        }
    }

    fn run(mut self) -> Used {
        let f = self.f;
        let jit = self.jit;
        if let Some(h) = jit.hle.get(&f.start) {
            // the runtime implements this function
            let name = format!("fepor_hle_{}", h.name);
            let r = self.import(&name, &[self.ptr], None);
            self.b.ins().call(r, &[self.ctx]);
            self.b.ins().return_(&[]);
            self.b.seal_all_blocks();
            let tg = self.module.target_config();
            self.b.finalize(tg);
            return self.used;
        }
        let words: Vec<(u32, Ins)> = (f.start..f.end)
            .step_by(4)
            .map(|a| {
                let w = self
                    .jit
                    .dol
                    .read_u32(a)
                    .unwrap_or_else(|| panic!("{a:08x} is outside the image"));
                (a, Ins::new(w))
            })
            .collect();
        let internal = |t: u32| t >= f.start && t < f.end;
        let mut labels: BTreeSet<u32> = BTreeSet::new();
        for &(a, ins) in &words {
            match ins.op {
                Opcode::B | Opcode::Bc if !ins.field_lk() => {
                    labels.extend(ins.branch_dest(a).filter(|&t| internal(t)));
                }
                Opcode::Bcctr if !ins.field_lk() => {
                    if let Some(t) = self.jit.tables.get(&a) {
                        labels.extend(t.targets.iter().copied().filter(|&t| internal(t)));
                    }
                }
                _ => {}
            }
        }
        // where a resuming HLE call (OSLoadContext) continues: after the first call of the matching save
        for &(a, ins) in &words {
            if ins.op == Opcode::B && ins.field_lk() {
                if let Some(t) = ins
                    .branch_dest(a)
                    .filter(|t| self.jit.save_targets.contains(t))
                {
                    self.resume_labels.entry(t).or_insert(a + 4);
                    labels.insert(a + 4);
                }
            }
        }
        // the resume variant continues after any call: the registers are in the context there, and the
        // host frames above are rebuilt from the guest stack (docs/adr/0035)
        let mut resumable: BTreeSet<u32> = BTreeSet::new();
        if self.resume {
            for &(a, ins) in &words {
                let call = match ins.op {
                    Opcode::B | Opcode::Bc | Opcode::Bclr | Opcode::Bcctr => ins.field_lk(),
                    _ => false,
                };
                if call && a + 4 <= f.end {
                    resumable.insert(a + 4);
                }
            }
            resumable.extend(labels.iter().copied());
            labels.extend(resumable.iter().copied());
        }
        labels.insert(f.start);
        for &l in &labels {
            let blk = self.b.create_block();
            self.blocks.insert(l, blk);
        }

        // entry: trace hook, registers in, then the first instruction
        if self.jit.trace_calls {
            let a = self.c32(f.start);
            self.call("fepor_enter", &[I32], None, &[a]);
        }
        self.inn();
        if self.resume {
            let at = self
                .b
                .ins()
                .load(I32, flags(), self.ctx, OFF_SPR + 4 * SPR_RESUME_PC);
            let mut sw = Switch::new();
            for &t in &resumable {
                sw.set_entry(t as u128, self.blocks[&t]);
            }
            let default = self.b.create_block();
            sw.emit(&mut self.b, at, default);
            self.b.switch_to_block(default);
            self.unresolved(f.start, at);
        } else {
            let first = self.blocks[&f.start];
            self.b.ins().jump(first, &[]);
            self.terminated = true;
        }

        for &(a, ins) in &words {
            self.begin(a);
            self.lift(a, ins);
        }
        // code that runs off the end of the extent continues in whatever follows
        self.begin(f.end);
        if self.jit.index.contains_key(&f.end) {
            self.out();
            self.call_function(f.end);
            self.terminate_with_return();
        } else {
            let t = self.c32(f.end);
            self.unresolved(f.end - 4, t);
        }
        self.b.seal_all_blocks();
        let tg = self.module.target_config();
        self.b.finalize(tg);
        self.used
    }
}
