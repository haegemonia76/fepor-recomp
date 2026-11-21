//! Per-instruction lifting. Registers live in C++ locals (`r[]`, `f[]`, `cr[]`, `lr`, `ctr`, `xer`,
//! `fpscr`); `in()`/`out()` lambdas sync them with the `Context` around calls and returns.

use std::collections::{BTreeSet, HashMap};

use fepor_core::analysis::JumpTable;
use fepor_core::config::HleEntry;
use ppc750cl::{Ins, Opcode};

#[derive(Default)]
pub struct Used {
    pub gpr: u32,
    pub fpr: u32,
    pub cr: bool,
    pub lr: bool,
    pub ctr: bool,
    pub xer: bool,
    pub fpscr: bool,
}

pub struct Fx<'a> {
    pub start: u32,
    pub end: u32,
    pub funcs: &'a BTreeSet<u32>,
    pub tables: &'a HashMap<u32, &'a JumpTable>,
    pub labels: BTreeSet<u32>,
    pub used: Used,
    pub body: Vec<String>,
    /// (address, instruction word) of instructions emitted as runtime traps.
    pub unimpl: Vec<(u32, u32)>,
    pub unimpl_ops: Vec<Opcode>,
    pub hle: &'a HashMap<u32, &'a HleEntry>,
    /// HLE save function address -> address after its first call in this function.
    pub resume_labels: std::collections::BTreeMap<u32, u32>,
}

fn h(v: u32) -> String {
    format!("0x{v:x}u")
}

fn simm(v: i16) -> String {
    h(v as i32 as u32)
}

fn mask(mb: u8, me: u8) -> u32 {
    let (mb, me) = (mb as u32, me as u32);
    let m = (u32::MAX >> mb) & (u32::MAX << (31 - me));
    if mb <= me {
        m
    } else {
        // wrapped mask: ones outside (me, mb)
        !((u32::MAX >> (me + 1)) & (u32::MAX << (32 - mb)))
    }
}

pub fn fname(addr: u32) -> String {
    format!("f_{addr:08x}")
}

impl<'a> Fx<'a> {
    fn r(&mut self, n: u8) -> String {
        self.used.gpr |= 1 << n;
        format!("r[{n}]")
    }
    /// `rA|0` operand.
    fn ra0(&mut self, n: u8) -> String {
        if n == 0 {
            "0u".into()
        } else {
            self.r(n)
        }
    }
    fn f(&mut self, n: u8) -> String {
        self.used.fpr |= 1 << n;
        format!("f[{n}]")
    }
    fn cr(&mut self, n: u8) -> String {
        self.used.cr = true;
        format!("cr[{n}]")
    }
    fn so(&mut self) -> &'static str {
        self.used.xer = true;
        "xer.so"
    }
    fn emit(&mut self, s: impl Into<String>) {
        self.body.push(s.into());
    }
    fn is_internal(&self, t: u32) -> bool {
        t >= self.start && t < self.end
    }

    /// `rA|0 + d`.
    fn ea_d(&mut self, a: u8, d: i16) -> String {
        match (a, d) {
            (0, d) => simm(d),
            (a, 0) => self.r(a),
            (a, d) => format!("({} + {})", self.r(a), simm(d)),
        }
    }
    /// `rA|0 + rB`.
    fn ea_x(&mut self, a: u8, b: u8) -> String {
        let rb = self.r(b);
        if a == 0 {
            rb
        } else {
            format!("({} + {rb})", self.r(a))
        }
    }

    fn rc0(&mut self, ins: Ins, dest: &str) {
        if ins.field_rc() {
            let so = self.so();
            self.emit(format!("cr[0] = rc0({dest}, {so});"));
            self.used.cr = true;
        }
    }

    fn rc1(&mut self, ins: Ins) {
        if ins.field_rc() {
            self.used.fpscr = true;
            self.used.cr = true;
            self.emit("cr[1] = CRF{(uint8_t)(fpscr >> 31 & 1), (uint8_t)(fpscr >> 30 & 1), (uint8_t)(fpscr >> 29 & 1), (uint8_t)(fpscr >> 28 & 1)};");
        }
    }

    fn jump(&mut self, from: u32, t: u32) -> Vec<String> {
        if self.is_internal(t) {
            let mut v = Vec::new();
            if t <= from {
                // loops need a safe point too: a guest spinning on a flag an interrupt sets never calls anything
                v.push("if (__builtin_expect(--fepor_ticks == 0, 0)) { out(); fepor_poll(&c); in(); }".to_string());
            }
            v.push(format!("goto L_{t:08x};"));
            v
        } else if self.funcs.contains(&t) {
            vec!["out();".into(), format!("[[clang::musttail]] return {}(c);", fname(t))]
        } else {
            vec!["out();".into(), format!("unresolved(c, 0x{from:x}, 0x{t:x});")]
        }
    }

    fn call(&mut self, from: u32, t: u32) -> Vec<String> {
        self.used.lr = true;
        if self.funcs.contains(&t) {
            let mut v = vec!["out();".to_string(), format!("{}(c);", fname(t)), "in();".to_string()];
            if let Some(h) = self.hle.get(&t) {
                if h.unwinds {
                    v.push("if (c.spr[1023] == 1) unwind();".into());
                }
                if let Some(label) = h.resumes_at.and_then(|save| self.resume_labels.get(&save)) {
                    v.push(format!("goto L_{label:08x};"));
                }
            }
            v
        } else {
            vec!["out();".into(), format!("unresolved(c, 0x{from:x}, 0x{t:x});")]
        }
    }

    /// Branch condition for BO/BI; emits the CTR decrement. `None` means always.
    fn cond(&mut self, bo: u8, bi: u8) -> Option<String> {
        let mut parts = Vec::new();
        if bo & 4 == 0 {
            self.used.ctr = true;
            self.emit("ctr -= 1;");
            parts.push(if bo & 2 == 0 { "ctr != 0" } else { "ctr == 0" }.to_string());
        }
        if bo & 0x10 == 0 {
            let f = ["lt", "gt", "eq", "so"][(bi & 3) as usize];
            let c = self.cr(bi >> 2);
            parts.push(if bo & 8 != 0 { format!("{c}.{f}") } else { format!("!{c}.{f}") });
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" && "))
        }
    }

    fn guarded(&mut self, cond: Option<String>, stmts: Vec<String>) {
        match cond {
            None => self.body.extend(stmts),
            Some(c) => {
                self.emit(format!("if ({c}) {{"));
                self.body.extend(stmts);
                self.emit("}");
            }
        }
    }

    fn indirect_tail(&mut self, from: u32, target: &str) -> Vec<String> {
        vec!["out();".into(), format!("[[clang::musttail]] return lookup(0x{from:x}, {target})(c);")]
    }

    fn branch(&mut self, addr: u32, ins: Ins) {
        let ret = addr + 4;
        let lk = ins.field_lk();
        let (bo, bi) = (ins.field_bo(), ins.field_bi());
        match ins.op {
            Opcode::B => {
                let t = ins.branch_dest(addr).unwrap_or(0);
                if lk {
                    self.emit(format!("lr = 0x{ret:x}u;"));
                    let s = self.call(addr, t);
                    self.body.extend(s);
                } else {
                    let s = self.jump(addr, t);
                    self.body.extend(s);
                }
            }
            Opcode::Bc => {
                let t = ins.branch_dest(addr).unwrap_or(0);
                let c = self.cond(bo, bi);
                if lk {
                    self.used.lr = true;
                    self.emit(format!("lr = 0x{ret:x}u;"));
                    if t != ret {
                        let s = self.call(addr, t);
                        self.guarded(c, s);
                    }
                } else {
                    let s = self.jump(addr, t);
                    self.guarded(c, s);
                }
            }
            Opcode::Bclr => {
                self.used.lr = true;
                if lk {
                    self.emit(format!("{{ uint32_t t = lr; lr = 0x{ret:x}u;"));
                    let c = self.cond(bo, bi);
                    let t = vec!["out();".into(), format!("lookup(0x{addr:x}, t)(c);"), "in();".into()];
                    self.guarded(c, t);
                    self.emit("}");
                } else {
                    let c = self.cond(bo, bi);
                    self.guarded(c, vec!["out();".into(), "return;".into()]);
                }
            }
            Opcode::Bcctr => {
                self.used.ctr = true;
                self.used.lr = true;
                if lk {
                    self.emit(format!("{{ uint32_t t = ctr; lr = 0x{ret:x}u;"));
                    let c = self.cond(bo, bi);
                    let t = vec!["out();".into(), format!("lookup(0x{addr:x}, t)(c);"), "in();".into()];
                    self.guarded(c, t);
                    self.emit("}");
                } else {
                    let c = self.cond(bo, bi);
                    let mut stmts = Vec::new();
                    if let Some(table) = self.tables.get(&addr).copied() {
                        stmts.push("switch (ctr) {".to_string());
                        let mut seen = BTreeSet::new();
                        for &t in &table.targets {
                            if seen.insert(t) {
                                stmts.push(format!("case 0x{t:x}u:"));
                                stmts.extend(self.jump(addr, t));
                            }
                        }
                        stmts.push("default:".into());
                        stmts.push("out();".into());
                        stmts.push(format!("unresolved(c, 0x{addr:x}, ctr);"));
                        stmts.push("}".into());
                    } else {
                        stmts = self.indirect_tail(addr, "ctr");
                    }
                    self.guarded(c, stmts);
                }
            }
            _ => unreachable!(),
        }
    }

    fn unimplemented(&mut self, addr: u32, ins: Ins) {
        self.unimpl.push((addr, ins.code));
        self.unimpl_ops.push(ins.op);
        self.emit(format!("out(); unimplemented(c, 0x{addr:x}, 0x{:08x});", ins.code));
    }

    fn load(&mut self, ins: Ins, expr: &str, ea: &str, update_reg: Option<u8>, fp: bool, kind: LoadKind) {
        let d = if fp { ins.field_frd() } else { ins.field_rd() };
        let dst = if fp { self.f(d) } else { self.r(d) };
        let assign = |dst: &str, v: &str| match kind {
            LoadKind::Gpr => format!("{dst} = {v};"),
            LoadKind::FprSingle => format!("{dst} = fill({v});"),
            LoadKind::FprDouble => format!("{dst}.x = {v};"),
            LoadKind::Quant => format!("{dst} = {v};"),
        };
        match update_reg {
            None => {
                let v = expr.replace("EA", ea);
                self.emit(assign(&dst, &v));
            }
            Some(a) => {
                let ra = self.r(a);
                let v = expr.replace("EA", "ea");
                self.emit(format!("{{ uint32_t ea = {ea}; {} {ra} = ea; }}", assign(&dst, &v)));
            }
        }
    }

    fn store(&mut self, expr: &str, ea: &str, update_reg: Option<u8>) {
        match update_reg {
            None => {
                let s = expr.replace("EA", ea);
                self.emit(format!("{s};"));
            }
            Some(a) => {
                let ra = self.r(a);
                let s = expr.replace("EA", "ea");
                self.emit(format!("{{ uint32_t ea = {ea}; {s}; {ra} = ea; }}"));
            }
        }
    }

    pub fn lift(&mut self, addr: u32, ins: Ins) {
        use Opcode::*;
        let d = ins.field_rd();
        let a = ins.field_ra();
        let b = ins.field_rb();
        let s = ins.field_rs();
        let sh = ins.field_sh();
        let oe = ins.field_oe();
        let fd = ins.field_frd();
        let fa = ins.field_fra();
        let fb = ins.field_frb();
        let fc = ins.field_frc();
        let fs = ins.field_frs();
        let uimm = ins.field_uimm();
        let simmv = ins.field_simm();
        let off = ins.field_offset();

        match ins.op {
            // ---- integer arithmetic ----
            Addi => {
                let (ra, rd) = (self.ra0(a), self.r(d));
                if a == 0 {
                    self.emit(format!("{rd} = {};", simm(simmv)));
                } else {
                    self.emit(format!("{rd} = {ra} + {};", simm(simmv)));
                }
            }
            Addis => {
                let (ra, rd) = (self.ra0(a), self.r(d));
                let imm = h((uimm as u32) << 16);
                if a == 0 {
                    self.emit(format!("{rd} = {imm};"));
                } else {
                    self.emit(format!("{rd} = {ra} + {imm};"));
                }
            }
            Addic | Addic_ => {
                self.used.xer = true;
                let (ra, rd) = (self.r(a), self.r(d));
                self.emit(format!("{rd} = addc(xer, {ra}, {});", simm(simmv)));
                if ins.op == Addic_ {
                    let so = self.so();
                    self.emit(format!("cr[0] = rc0({rd}, {so});"));
                    self.used.cr = true;
                }
            }
            Subfic => {
                self.used.xer = true;
                let (ra, rd) = (self.r(a), self.r(d));
                self.emit(format!("{rd} = subfc(xer, {ra}, {});", simm(simmv)));
            }
            Mulli => {
                let (ra, rd) = (self.r(a), self.r(d));
                self.emit(format!("{rd} = mullw({ra}, {});", simm(simmv)));
            }
            Add | Subf | Neg | Mullw | Divw | Divwu | Mulhw | Mulhwu | Addc | Adde | Addze | Addme | Subfc | Subfe | Subfze | Subfme => {
                let carry_op = matches!(ins.op, Addc | Adde | Addze | Addme | Subfc | Subfe | Subfze | Subfme);
                let mulh = matches!(ins.op, Mulhw | Mulhwu);
                if (oe && (carry_op || mulh)) || (ins.op == Mulhw && oe) {
                    return self.unimplemented(addr, ins);
                }
                if carry_op || oe {
                    self.used.xer = true;
                }
                let (ra, rb, rd) = (self.r(a), self.r(b), self.r(d));
                let e = match (ins.op, oe) {
                    (Add, false) => format!("{ra} + {rb}"),
                    (Add, true) => format!("add_o(xer, {ra}, {rb})"),
                    (Subf, false) => format!("{rb} - {ra}"),
                    (Subf, true) => format!("subf_o(xer, {ra}, {rb})"),
                    (Neg, false) => format!("0u - {ra}"),
                    (Neg, true) => format!("neg_o(xer, {ra})"),
                    (Mullw, false) => format!("mullw({ra}, {rb})"),
                    (Mullw, true) => format!("mullw_o(xer, {ra}, {rb})"),
                    (Divw, false) => format!("divw({ra}, {rb})"),
                    (Divw, true) => format!("divw_o(xer, {ra}, {rb})"),
                    (Divwu, false) => format!("divwu({ra}, {rb})"),
                    (Divwu, true) => format!("divwu_o(xer, {ra}, {rb})"),
                    (Mulhw, _) => format!("mulhw({ra}, {rb})"),
                    (Mulhwu, _) => format!("mulhwu({ra}, {rb})"),
                    (Addc, _) => format!("addc(xer, {ra}, {rb})"),
                    (Adde, _) => format!("adde(xer, {ra}, {rb})"),
                    (Addze, _) => format!("addze(xer, {ra})"),
                    (Addme, _) => format!("addme(xer, {ra})"),
                    (Subfc, _) => format!("subfc(xer, {ra}, {rb})"),
                    (Subfe, _) => format!("subfe(xer, {ra}, {rb})"),
                    (Subfze, _) => format!("subfze(xer, {ra})"),
                    (Subfme, _) => format!("subfme(xer, {ra})"),
                    _ => unreachable!(),
                };
                self.emit(format!("{rd} = {e};"));
                self.rc0(ins, &rd);
            }
            // ---- logical ----
            And | Andc | Or | Orc | Xor | Nand | Nor | Eqv => {
                let (rs, rb, ra) = (self.r(s), self.r(b), self.r(a));
                let e = match ins.op {
                    And => format!("{rs} & {rb}"),
                    Andc => format!("{rs} & ~{rb}"),
                    Or => format!("{rs} | {rb}"),
                    Orc => format!("{rs} | ~{rb}"),
                    Xor => format!("{rs} ^ {rb}"),
                    Nand => format!("~({rs} & {rb})"),
                    Nor => format!("~({rs} | {rb})"),
                    _ => format!("~({rs} ^ {rb})"),
                };
                self.emit(format!("{ra} = {e};"));
                self.rc0(ins, &ra);
            }
            Ori | Oris | Xori | Xoris | Andi_ | Andis_ => {
                let imm = match ins.op {
                    Oris | Xoris | Andis_ => h((uimm as u32) << 16),
                    _ => h(uimm as u32),
                };
                let op = match ins.op {
                    Ori | Oris => '|',
                    Xori | Xoris => '^',
                    _ => '&',
                };
                if ins.op == Ori && uimm == 0 && a == 0 && s == 0 {
                    self.emit(";"); // nop
                } else {
                    let (rs, ra) = (self.r(s), self.r(a));
                    self.emit(format!("{ra} = {rs} {op} {imm};"));
                    if matches!(ins.op, Andi_ | Andis_) {
                        let so = self.so();
                        self.emit(format!("cr[0] = rc0({ra}, {so});"));
                        self.used.cr = true;
                    }
                }
            }
            Extsb | Extsh | Cntlzw => {
                let (rs, ra) = (self.r(s), self.r(a));
                let f = match ins.op {
                    Extsb => "extsb",
                    Extsh => "extsh",
                    _ => "cntlzw",
                };
                self.emit(format!("{ra} = {f}({rs});"));
                self.rc0(ins, &ra);
            }
            Slw | Srw => {
                let (rs, rb, ra) = (self.r(s), self.r(b), self.r(a));
                let f = if ins.op == Slw { "slw" } else { "srw" };
                self.emit(format!("{ra} = {f}({rs}, {rb});"));
                self.rc0(ins, &ra);
            }
            Sraw | Srawi => {
                self.used.xer = true;
                let (rs, ra) = (self.r(s), self.r(a));
                let amount = if ins.op == Sraw { self.r(b) } else { format!("{sh}") };
                self.emit(format!("{ra} = sraw(xer, {rs}, {amount});"));
                self.rc0(ins, &ra);
            }
            Rlwinm | Rlwimi | Rlwnm => {
                let m = mask(ins.field_mb(), ins.field_me());
                let (rs, ra) = (self.r(s), self.r(a));
                let rot = if ins.op == Rlwnm {
                    format!("rotl32({rs}, {})", self.r(b))
                } else if sh == 0 {
                    rs.clone()
                } else {
                    format!("rotl32({rs}, {sh})")
                };
                let e = if ins.op == Rlwimi {
                    format!("({rot} & {}) | ({ra} & {})", h(m), h(!m))
                } else if m == u32::MAX {
                    rot
                } else {
                    format!("{rot} & {}", h(m))
                };
                self.emit(format!("{ra} = {e};"));
                self.rc0(ins, &ra);
            }
            // ---- compare ----
            Cmp | Cmpl | Cmpi | Cmpli => {
                let crd = self.cr(ins.field_crfd());
                let so = self.so();
                let ra = self.r(a);
                let e = match ins.op {
                    Cmp => format!("cmp_s({ra}, {}, {so})", self.r(b)),
                    Cmpl => format!("cmp_u({ra}, {}, {so})", self.r(b)),
                    Cmpi => format!("cmp_s({ra}, {}, {so})", simmv),
                    _ => format!("cmp_u({ra}, {}u, {so})", uimm),
                };
                self.emit(format!("{crd} = {e};"));
            }
            // ---- integer loads ----
            Lbz | Lbzu | Lhz | Lhzu | Lha | Lhau | Lwz | Lwzu => {
                let upd = matches!(ins.op, Lbzu | Lhzu | Lhau | Lwzu).then_some(a);
                let ea = self.ea_d(a, off);
                let e = match ins.op {
                    Lbz | Lbzu => "(uint32_t)ld8(m, EA)",
                    Lhz | Lhzu => "(uint32_t)ld16(m, EA)",
                    Lha | Lhau => "(uint32_t)(int32_t)(int16_t)ld16(m, EA)",
                    _ => "ld32(m, EA)",
                };
                self.load(ins, e, &ea, upd, false, LoadKind::Gpr);
            }
            Lbzx | Lbzux | Lhzx | Lhzux | Lhax | Lhaux | Lwzx | Lwzux | Lhbrx | Lwbrx => {
                let upd = matches!(ins.op, Lbzux | Lhzux | Lhaux | Lwzux).then_some(a);
                let ea = self.ea_x(a, b);
                let e = match ins.op {
                    Lbzx | Lbzux => "(uint32_t)ld8(m, EA)",
                    Lhzx | Lhzux => "(uint32_t)ld16(m, EA)",
                    Lhax | Lhaux => "(uint32_t)(int32_t)(int16_t)ld16(m, EA)",
                    Lhbrx => "(uint32_t)ld16br(m, EA)",
                    Lwbrx => "ld32br(m, EA)",
                    _ => "ld32(m, EA)",
                };
                self.load(ins, e, &ea, upd, false, LoadKind::Gpr);
            }
            Lwarx => {
                let ea = self.ea_x(a, b);
                let rd = self.r(d);
                self.emit(format!("{{ uint32_t ea = {ea}; {rd} = ld32(m, ea); c.reserve_addr = ea; }}"));
            }
            Lmw => {
                let mut ea = self.ea_d(a, off);
                self.emit(format!("{{ uint32_t ea = {ea};"));
                for k in d..32 {
                    ea = "ea".into();
                    let rk = self.r(k);
                    self.emit(format!("{rk} = ld32(m, {ea}); ea += 4;"));
                }
                self.emit("}");
            }
            Stmw => {
                let ea = self.ea_d(a, off);
                self.emit(format!("{{ uint32_t ea = {ea};"));
                for k in s..32 {
                    let rk = self.r(k);
                    self.emit(format!("st32(m, ea, {rk}); ea += 4;"));
                }
                self.emit("}");
            }
            // ---- integer stores ----
            Stb | Stbu | Sth | Sthu | Stw | Stwu => {
                let upd = matches!(ins.op, Stbu | Sthu | Stwu).then_some(a);
                let ea = self.ea_d(a, off);
                let rs = self.r(s);
                let e = match ins.op {
                    Stb | Stbu => format!("st8(m, EA, (uint8_t){rs})"),
                    Sth | Sthu => format!("st16(m, EA, (uint16_t){rs})"),
                    _ => format!("st32(m, EA, {rs})"),
                };
                self.store(&e, &ea, upd);
            }
            Stbx | Stbux | Sthx | Sthux | Stwx | Stwux | Sthbrx | Stwbrx => {
                let upd = matches!(ins.op, Stbux | Sthux | Stwux).then_some(a);
                let ea = self.ea_x(a, b);
                let rs = self.r(s);
                let e = match ins.op {
                    Stbx | Stbux => format!("st8(m, EA, (uint8_t){rs})"),
                    Sthx | Sthux => format!("st16(m, EA, (uint16_t){rs})"),
                    Sthbrx => format!("st16br(m, EA, (uint16_t){rs})"),
                    Stwbrx => format!("st32br(m, EA, {rs})"),
                    _ => format!("st32(m, EA, {rs})"),
                };
                self.store(&e, &ea, upd);
            }
            Stwcx_ => {
                let ea = self.ea_x(a, b);
                let rs = self.r(s);
                let so = self.so();
                self.used.cr = true;
                self.emit(format!("{{ st32(m, {ea}, {rs}); cr[0] = CRF{{0, 0, 1, {so}}}; }}"));
            }
            // ---- FP loads/stores ----
            Lfs | Lfsu | Lfd | Lfdu => {
                let upd = matches!(ins.op, Lfsu | Lfdu).then_some(a);
                let ea = self.ea_d(a, off);
                let (e, k) = if matches!(ins.op, Lfs | Lfsu) { ("ldfs(m, EA)", LoadKind::FprSingle) } else { ("ldfd(m, EA)", LoadKind::FprDouble) };
                self.load(ins, e, &ea, upd, true, k);
            }
            Lfsx | Lfsux | Lfdx | Lfdux => {
                let upd = matches!(ins.op, Lfsux | Lfdux).then_some(a);
                let ea = self.ea_x(a, b);
                let (e, k) = if matches!(ins.op, Lfsx | Lfsux) { ("ldfs(m, EA)", LoadKind::FprSingle) } else { ("ldfd(m, EA)", LoadKind::FprDouble) };
                self.load(ins, e, &ea, upd, true, k);
            }
            Stfs | Stfsu | Stfd | Stfdu => {
                let upd = matches!(ins.op, Stfsu | Stfdu).then_some(a);
                let ea = self.ea_d(a, off);
                let f = self.f(fs);
                let e = if matches!(ins.op, Stfs | Stfsu) { format!("stfs(m, EA, {f}.x)") } else { format!("stfd(m, EA, {f}.x)") };
                self.store(&e, &ea, upd);
            }
            Stfsx | Stfsux | Stfdx | Stfdux | Stfiwx => {
                let upd = matches!(ins.op, Stfsux | Stfdux).then_some(a);
                let ea = self.ea_x(a, b);
                let f = self.f(fs);
                let e = match ins.op {
                    Stfsx | Stfsux => format!("stfs(m, EA, {f}.x)"),
                    Stfiwx => format!("stfiwx(m, EA, {f}.x)"),
                    _ => format!("stfd(m, EA, {f}.x)"),
                };
                self.store(&e, &ea, upd);
            }
            PsqL | PsqLu => {
                let (i, w) = (ins.field_ps_i(), ins.field_ps_w());
                let ea = self.ea_d(a, ins.field_ps_offset());
                let e = format!("psq_ld(m, EA, c.spr[{}], {w})", 912 + i as u32);
                self.load(ins, &e, &ea, (ins.op == PsqLu).then_some(a), true, LoadKind::Quant);
            }
            PsqLx | PsqLux => {
                let (i, w) = (ins.field_ps_ix(), ins.field_ps_wx());
                let ea = self.ea_x(a, b);
                let e = format!("psq_ld(m, EA, c.spr[{}], {w})", 912 + i as u32);
                self.load(ins, &e, &ea, (ins.op == PsqLux).then_some(a), true, LoadKind::Quant);
            }
            PsqSt | PsqStu => {
                let (i, w) = (ins.field_ps_i(), ins.field_ps_w());
                let ea = self.ea_d(a, ins.field_ps_offset());
                let f = self.f(fs);
                let e = format!("psq_st(m, EA, c.spr[{}], {w}, {f})", 912 + i as u32);
                self.store(&e, &ea, (ins.op == PsqStu).then_some(a));
            }
            PsqStx | PsqStux => {
                let (i, w) = (ins.field_ps_ix(), ins.field_ps_wx());
                let ea = self.ea_x(a, b);
                let f = self.f(fs);
                let e = format!("psq_st(m, EA, c.spr[{}], {w}, {f})", 912 + i as u32);
                self.store(&e, &ea, (ins.op == PsqStux).then_some(a));
            }
            // ---- branches ----
            B | Bc | Bclr | Bcctr => self.branch(addr, ins),
            // ---- condition register ----
            Crand | Crandc | Creqv | Crnand | Crnor | Cror | Crorc | Crxor => {
                self.used.cr = true;
                let (x, y) = (format!("crbit_get(cr, {})", ins.field_crba()), format!("crbit_get(cr, {})", ins.field_crbb()));
                let e = match ins.op {
                    Crand => format!("{x} & {y}"),
                    Crandc => format!("{x} & !{y}"),
                    Creqv => format!("!({x} ^ {y})"),
                    Crnand => format!("!({x} & {y})"),
                    Crnor => format!("!({x} | {y})"),
                    Cror => format!("{x} | {y}"),
                    Crorc => format!("{x} | !{y}"),
                    _ => format!("{x} ^ {y}"),
                };
                self.emit(format!("crbit_set(cr, {}, {e});", ins.field_crbd()));
            }
            Mcrf => {
                let (x, y) = (self.cr(ins.field_crfd()), self.cr(ins.field_crfs()));
                self.emit(format!("{x} = {y};"));
            }
            Mcrxr => {
                self.used.xer = true;
                let x = self.cr(ins.field_crfd());
                self.emit(format!("{x} = CRF{{xer.so, xer.ov, xer.ca, 0}}; xer.so = xer.ov = xer.ca = 0;"));
            }
            Mfcr => {
                self.used.cr = true;
                let rd = self.r(d);
                self.emit(format!("{rd} = mfcr(cr);"));
            }
            Mtcrf => {
                self.used.cr = true;
                let rs = self.r(s);
                self.emit(format!("mtcrf(cr, {}, {rs});", ins.field_crm()));
            }
            // ---- special registers ----
            Mfspr => {
                let rd = self.r(d);
                let spr = ins.field_spr();
                let e = match spr {
                    1 => {
                        self.used.xer = true;
                        "xer_pack(xer)".to_string()
                    }
                    8 => {
                        self.used.lr = true;
                        "lr".to_string()
                    }
                    9 => {
                        self.used.ctr = true;
                        "ctr".to_string()
                    }
                    n => format!("c.spr[{n}]"),
                };
                self.emit(format!("{rd} = {e};"));
            }
            Mtspr => {
                let rs = self.r(s);
                match ins.field_spr() {
                    1 => {
                        self.used.xer = true;
                        self.emit(format!("xer = xer_unpack({rs});"));
                    }
                    8 => {
                        self.used.lr = true;
                        self.emit(format!("lr = {rs};"));
                    }
                    9 => {
                        self.used.ctr = true;
                        self.emit(format!("ctr = {rs};"));
                    }
                    n => self.emit(format!("c.spr[{n}] = {rs};")),
                }
            }
            Mftb => {
                let rd = self.r(d);
                self.emit(format!("{rd} = mftb({});", ins.field_tbr()));
            }
            Mfmsr => {
                let rd = self.r(d);
                self.emit(format!("{rd} = c.msr;"));
            }
            Mtmsr => {
                let rs = self.r(s);
                self.emit(format!("c.msr = {rs};"));
            }
            Mfsr => {
                let rd = self.r(d);
                self.emit(format!("{rd} = c.sr[{}];", ins.field_sr()));
            }
            Mtsr => {
                let rs = self.r(s);
                self.emit(format!("c.sr[{}] = {rs};", ins.field_sr()));
            }
            Mfsrin => {
                let (rd, rb) = (self.r(d), self.r(b));
                self.emit(format!("{rd} = c.sr[{rb} >> 28];"));
            }
            Mtsrin => {
                let (rs, rb) = (self.r(s), self.r(b));
                self.emit(format!("c.sr[{rb} >> 28] = {rs};"));
            }
            // ---- FPSCR ----
            Mffs => {
                self.used.fpscr = true;
                let f = self.f(fd);
                self.emit(format!("{f}.x = std::bit_cast<double>(0xFFF8000000000000ull | fpscr);"));
                self.rc1(ins);
            }
            Mtfsf => {
                self.used.fpscr = true;
                let fm = ins.field_mtfsf_fm();
                let mut m = 0u32;
                for i in 0..8 {
                    if fm & (0x80 >> i) != 0 {
                        m |= 0xF << (28 - 4 * i);
                    }
                }
                let f = self.f(fb);
                self.emit(format!("fpscr = (fpscr & ~{}) | ((uint32_t)std::bit_cast<uint64_t>({f}.x) & {});", h(m), h(m)));
                self.rc1(ins);
            }
            Mtfsfi => {
                self.used.fpscr = true;
                let n = ins.field_crfd() as u32;
                let m = 0xFu32 << (28 - 4 * n);
                self.emit(format!("fpscr = (fpscr & ~{}) | {};", h(m), h((ins.field_mtfsf_imm() as u32) << (28 - 4 * n))));
                self.rc1(ins);
            }
            Mtfsb0 | Mtfsb1 => {
                self.used.fpscr = true;
                let m = 0x8000_0000u32 >> ins.field_crbd();
                if ins.op == Mtfsb0 {
                    self.emit(format!("fpscr &= ~{};", h(m)));
                } else {
                    self.emit(format!("fpscr |= {};", h(m)));
                }
                self.rc1(ins);
            }
            Mcrfs => {
                self.used.fpscr = true;
                let n = ins.field_crfs() as u32;
                let c = self.cr(ins.field_crfd());
                self.emit(format!("{{ uint32_t v = fpscr >> {} & 0xF; {c} = CRF{{(uint8_t)(v >> 3 & 1), (uint8_t)(v >> 2 & 1), (uint8_t)(v >> 1 & 1), (uint8_t)(v & 1)}}; }}", 28 - 4 * n));
            }
            // ---- cache, sync, system ----
            Sync | Isync | Eieio | Dcbf | Dcbi | Dcbst | Dcbt | Dcbtst | Icbi | Tlbie | Tlbsync => self.emit(";"),
            Dcbz | DcbzL => {
                let ea = self.ea_x(a, b);
                self.emit(format!("dcbz(m, {ea});"));
            }
            Twi | Tw => {
                let to = ins.field_to();
                if to == 0 {
                    self.emit(";");
                } else {
                    let ra = self.r(a);
                    let rhs = if ins.op == Twi { simm(simmv) } else { self.r(b) };
                    self.emit(format!("if (tw_cond({to}, {ra}, {rhs})) {{ out(); trap(c, 0x{addr:x}); }}"));
                }
            }
            // `sc` only reaches the SDK's instruction-cache flush handler; the runtime may treat it as a no-op.
            Sc => self.emit(format!("out(); syscall(c, 0x{addr:x}); in();")),
            Rfi => self.emit(format!("out(); rfi(c, 0x{addr:x}); return;")),
            // ---- floating point ----
            Fmr | Fabs | Fneg | Fnabs => {
                let (x, y) = (self.f(fd), self.f(fb));
                let e = match ins.op {
                    Fmr => format!("{y}.x"),
                    Fabs => format!("std::fabs({y}.x)"),
                    Fneg => format!("-{y}.x"),
                    _ => format!("-std::fabs({y}.x)"),
                };
                self.emit(format!("{x}.x = {e};"));
                self.rc1(ins);
            }
            Fadd | Fsub | Fmul | Fdiv | Fadds | Fsubs | Fmuls | Fdivs => {
                let (x, ra, rb, rc) = (self.f(fd), self.f(fa), self.f(fb), self.f(fc));
                let (e, single) = match ins.op {
                    Fadd => (format!("pfadd({ra}.x, {rb}.x)"), false),
                    Fsub => (format!("pfsub({ra}.x, {rb}.x)"), false),
                    Fmul => (format!("pfmul({ra}.x, {rc}.x)"), false),
                    Fdiv => (format!("pfdiv({ra}.x, {rb}.x)"), false),
                    Fadds => (format!("pfadd({ra}.x, {rb}.x)"), true),
                    Fsubs => (format!("pfsub({ra}.x, {rb}.x)"), true),
                    Fmuls => (format!("pfmul({ra}.x, force25({rc}.x))"), true),
                    _ => (format!("pfdiv({ra}.x, {rb}.x)"), true),
                };
                if single {
                    self.emit(format!("{x} = fill(round_single({e}));"));
                } else {
                    self.emit(format!("{x}.x = {e};"));
                }
                self.rc1(ins);
            }
            Fmadd | Fmsub | Fnmadd | Fnmsub | Fmadds | Fmsubs | Fnmadds | Fnmsubs => {
                let (x, ra, rb, rc) = (self.f(fd), self.f(fa), self.f(fb), self.f(fc));
                let single = matches!(ins.op, Fmadds | Fmsubs | Fnmadds | Fnmsubs);
                let c = if single { format!("force25({rc}.x)") } else { format!("{rc}.x") };
                let (op, neg) = match ins.op {
                    Fmadd | Fmadds => ("pfmadd", false),
                    Fmsub | Fmsubs => ("pfmsub", false),
                    Fnmadd | Fnmadds => ("pfmadd", true),
                    _ => ("pfmsub", true),
                };
                let mut e = format!("{op}({ra}.x, {c}, {rb}.x)");
                if single {
                    e = format!("round_single({e})");
                }
                if neg {
                    e = format!("neg_nonan({e})");
                }
                if single {
                    self.emit(format!("{x} = fill({e});"));
                } else {
                    self.emit(format!("{x}.x = {e};"));
                }
                self.rc1(ins);
            }
            Fres => {
                let (x, rb) = (self.f(fd), self.f(fb));
                self.emit(format!("{x} = fill(fres({rb}.x));"));
                self.rc1(ins);
            }
            Frsqrte => {
                let (x, rb) = (self.f(fd), self.f(fb));
                self.emit(format!("{x}.x = frsqrte({rb}.x);"));
                self.rc1(ins);
            }
            Frsp => {
                let (x, rb) = (self.f(fd), self.f(fb));
                self.emit(format!("{x} = fill(round_single({rb}.x));"));
                self.rc1(ins);
            }
            Fsel => {
                let (x, ra, rb, rc) = (self.f(fd), self.f(fa), self.f(fb), self.f(fc));
                self.emit(format!("{x}.x = fsel({ra}.x, {rb}.x, {rc}.x);"));
                self.rc1(ins);
            }
            Fctiw | Fctiwz => {
                let (x, rb) = (self.f(fd), self.f(fb));
                let mode = if ins.op == Fctiwz {
                    "1".to_string()
                } else {
                    self.used.fpscr = true;
                    "fpscr & 3".to_string()
                };
                self.emit(format!("{x}.x = fctiw({rb}.x, {mode});"));
                self.rc1(ins);
            }
            Fcmpu | Fcmpo => {
                let crd = self.cr(ins.field_crfd());
                let (ra, rb) = (self.f(fa), self.f(fb));
                self.emit(format!("{crd} = fcmp({ra}.x, {rb}.x);"));
            }
            // ---- paired single ----
            PsAdd | PsSub | PsMul | PsDiv | PsMadd | PsMsub | PsNmadd | PsNmsub => {
                let (x, ra, rb, rc) = (self.f(fd), self.f(fa), self.f(fb), self.f(fc));
                let e = match ins.op {
                    PsAdd => format!("ps_add({ra}, {rb})"),
                    PsSub => format!("ps_sub({ra}, {rb})"),
                    PsMul => format!("ps_mul({ra}, {rc})"),
                    PsDiv => format!("ps_div({ra}, {rb})"),
                    PsMadd => format!("ps_madd({ra}, {rc}, {rb})"),
                    PsMsub => format!("ps_msub({ra}, {rc}, {rb})"),
                    PsNmadd => format!("neg_nonan(ps_madd({ra}, {rc}, {rb}))"),
                    _ => format!("neg_nonan(ps_msub({ra}, {rc}, {rb}))"),
                };
                self.emit(format!("{x} = {e};"));
                self.rc1(ins);
            }
            PsMuls0 | PsMuls1 | PsMadds0 | PsMadds1 => {
                let (x, ra, rb, rc) = (self.f(fd), self.f(fa), self.f(fb), self.f(fc));
                let lane = if matches!(ins.op, PsMuls0 | PsMadds0) { "xx" } else { "yy" };
                let e = if matches!(ins.op, PsMuls0 | PsMuls1) {
                    format!("ps_mul({ra}, {rc}.{lane})")
                } else {
                    format!("ps_madd({ra}, {rc}.{lane}, {rb})")
                };
                self.emit(format!("{x} = {e};"));
                self.rc1(ins);
            }
            PsSum0 | PsSum1 => {
                let (x, ra, rb, rc) = (self.f(fd), self.f(fa), self.f(fb), self.f(fc));
                let sum = format!("round_single(pfadd({ra}.x, {rb}.y))");
                let e = if ins.op == PsSum0 { format!("v2d{{{sum}, {rc}.y}}") } else { format!("v2d{{{rc}.x, {sum}}}") };
                self.emit(format!("{x} = {e};"));
                self.rc1(ins);
            }
            PsMerge00 | PsMerge01 | PsMerge10 | PsMerge11 => {
                let (x, ra, rb) = (self.f(fd), self.f(fa), self.f(fb));
                let (l, r) = match ins.op {
                    PsMerge00 => ("x", "x"),
                    PsMerge01 => ("x", "y"),
                    PsMerge10 => ("y", "x"),
                    _ => ("y", "y"),
                };
                self.emit(format!("{x} = v2d{{{ra}.{l}, {rb}.{r}}};"));
                self.rc1(ins);
            }
            PsMr | PsNeg | PsAbs | PsNabs | PsRes | PsRsqrte => {
                let (x, rb) = (self.f(fd), self.f(fb));
                let e = match ins.op {
                    PsMr => rb.clone(),
                    PsNeg => format!("-{rb}"),
                    PsAbs => format!("v2d{{std::fabs({rb}.x), std::fabs({rb}.y)}}"),
                    PsNabs => format!("v2d{{-std::fabs({rb}.x), -std::fabs({rb}.y)}}"),
                    PsRes => format!("ps_res({rb})"),
                    _ => format!("ps_rsqrte({rb})"),
                };
                self.emit(format!("{x} = {e};"));
                self.rc1(ins);
            }
            PsSel => {
                let (x, ra, rb, rc) = (self.f(fd), self.f(fa), self.f(fb), self.f(fc));
                self.emit(format!("{x} = v2d{{fsel({ra}.x, {rb}.x, {rc}.x), fsel({ra}.y, {rb}.y, {rc}.y)}};"));
                self.rc1(ins);
            }
            PsCmpu0 | PsCmpo0 | PsCmpu1 | PsCmpo1 => {
                let crd = self.cr(ins.field_crfd());
                let (ra, rb) = (self.f(fa), self.f(fb));
                let l = if matches!(ins.op, PsCmpu0 | PsCmpo0) { "x" } else { "y" };
                self.emit(format!("{crd} = fcmp({ra}.{l}, {rb}.{l});"));
            }
            _ => self.unimplemented(addr, ins),
        }
    }
}

#[derive(Clone, Copy)]
enum LoadKind {
    Gpr,
    FprSingle,
    FprDouble,
    Quant,
}

#[cfg(test)]
mod tests {
    use super::mask;

    #[test]
    fn rlwinm_masks_including_wraparound() {
        assert_eq!(mask(0, 31), 0xFFFF_FFFF);
        assert_eq!(mask(16, 31), 0x0000_FFFF);
        assert_eq!(mask(0, 15), 0xFFFF_0000);
        assert_eq!(mask(31, 31), 1);
        assert_eq!(mask(31, 0), 0x8000_0001);
        assert_eq!(mask(28, 3), 0xF000_000F);
    }
}
