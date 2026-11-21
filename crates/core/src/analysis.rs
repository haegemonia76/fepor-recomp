//! Static analysis of the DOL text: function starts, extents, jump tables and indirect branches.
//!
//! Without symbols, starts come from (in order of trust): the entry point and config, `bl` targets,
//! tail calls, pointers in data and `lis`+`addi/ori` address loads that land in uncovered code, and
//! finally the first instruction of any remaining unreached code run (`Origin::Gap`).
//! Discovery iterates to a fixpoint because boundaries change which branches count as internal.

use std::collections::{BTreeMap, BTreeSet};

use ppc750cl::{Argument, Ins, Offset, Opcode, ParsedIns, Simm, Uimm, GPR, SPR};

use crate::config::AnalysisConfig;
use crate::dol::Dol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Entry,
    Config,
    /// Target of `bl`.
    Call,
    /// Target of a `b`/`bc` that leaves the calling function.
    TailCall,
    /// Word in a data section pointing at uncovered code.
    DataPointer,
    /// `lis` + `addi`/`ori` constant pointing at uncovered code.
    AddressLoad,
    /// First instruction of a run of code no other start reaches.
    Gap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub start: u32,
    /// Exclusive end: one past the highest reachable instruction.
    pub end: u32,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSource {
    /// Table bounded by a preceding `cmplwi`.
    Auto,
    /// No bound found; entries read while they stay valid code addresses.
    AutoUnbounded,
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpTable {
    /// Address of the `bctr`.
    pub branch: u32,
    pub function: u32,
    pub table: Option<u32>,
    pub targets: Vec<u32>,
    pub source: TableSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndirectKind {
    /// `bctr` with no resolved jump table (indirect tail call or unknown switch): needs runtime dispatch or config.
    UnresolvedJump,
    /// `bctrl`: indirect call, resolved by the runtime dispatch table.
    CallCtr,
    /// `blrl`.
    CallLr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndirectBranch {
    pub addr: u32,
    pub function: u32,
    pub kind: IndirectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Default)]
pub struct Analysis {
    /// Sorted by start.
    pub functions: Vec<Function>,
    pub jump_tables: Vec<JumpTable>,
    pub indirect: Vec<IndirectBranch>,
    /// Non-padding code bytes no function reaches (likely data in text, or code behind an illegal word).
    pub unreached: Vec<Range>,
    /// Untrusted starts dropped because preceding code runs into them (mid-function labels).
    pub rejected: Vec<(u32, Origin)>,
    pub warnings: Vec<String>,
    pub text_bytes: u32,
    pub covered_bytes: u32,
}

struct Code {
    addr: Vec<u32>,
    ins: Vec<Ins>,
    allowed: Vec<bool>,
    /// (base, end, first index) per text section.
    secs: Vec<(u32, u32, usize)>,
}

impl Code {
    fn build(dol: &Dol, cfg: &AnalysisConfig) -> Code {
        let mut c = Code { addr: vec![], ins: vec![], allowed: vec![], secs: vec![] };
        for s in dol.text_sections() {
            c.secs.push((s.addr, s.addr + s.size, c.ins.len()));
            for (k, w) in dol.section_bytes(s).chunks_exact(4).enumerate() {
                let a = s.addr + 4 * k as u32;
                c.addr.push(a);
                c.ins.push(Ins::new(u32::from_be_bytes(w.try_into().unwrap())));
                c.allowed.push(!cfg.exclude.iter().any(|r| a >= r.start && a < r.end));
            }
        }
        c
    }

    fn idx(&self, a: u32) -> Option<usize> {
        if a & 3 != 0 {
            return None;
        }
        let &(base, _, first) = self.secs.iter().find(|&&(b, e, _)| a >= b && a < e)?;
        let i = first + ((a - base) / 4) as usize;
        self.allowed[i].then_some(i)
    }

    /// Index of the instruction at the next address, if it is contiguous code.
    fn next(&self, i: usize) -> Option<usize> {
        let j = i + 1;
        (j < self.ins.len() && self.allowed[j] && self.addr[j] == self.addr[i] + 4).then_some(j)
    }

    fn valid(&self, a: u32) -> bool {
        self.idx(a).is_some_and(|i| self.ins[i].op != Opcode::Illegal)
    }
}

fn gpr(p: &ParsedIns, k: usize) -> Option<u8> {
    match p.args[k] {
        Argument::GPR(GPR(r)) => Some(r),
        _ => None,
    }
}

fn uimm(p: &ParsedIns, k: usize) -> Option<u16> {
    match p.args[k] {
        Argument::Uimm(Uimm(v)) => Some(v),
        _ => None,
    }
}

fn simm(p: &ParsedIns, k: usize) -> Option<i16> {
    match p.args[k] {
        Argument::Simm(Simm(v)) => Some(v),
        _ => None,
    }
}

fn offset(p: &ParsedIns, k: usize) -> Option<i16> {
    match p.args[k] {
        Argument::Offset(Offset(v)) => Some(v),
        _ => None,
    }
}

fn defines(ins: Ins, reg: u8) -> bool {
    ins.defs().iter().any(|a| matches!(a, Argument::GPR(GPR(r)) if *r == reg))
}

/// Constant value of `reg` just before instruction `at`, following `lis`/`addi`/`ori` chains.
fn const_val(code: &Code, at: usize, lo: usize, reg: u8, depth: u8) -> Option<u32> {
    if depth == 0 {
        return None;
    }
    let j = (lo..at).rev().find(|&j| defines(code.ins[j], reg))?;
    let ins = code.ins[j];
    let p = ins.basic();
    match ins.op {
        Opcode::Addis => {
            let hi = (uimm(&p, 2)? as u32) << 16;
            match gpr(&p, 1)? {
                0 => Some(hi),
                ra => Some(const_val(code, j, lo, ra, depth - 1)?.wrapping_add(hi)),
            }
        }
        Opcode::Addi => {
            let lo16 = simm(&p, 2)? as i32 as u32;
            match gpr(&p, 1)? {
                0 => Some(lo16),
                ra => Some(const_val(code, j, lo, ra, depth - 1)?.wrapping_add(lo16)),
            }
        }
        Opcode::Ori => Some(const_val(code, j, lo, gpr(&p, 1)?, depth - 1)? | uimm(&p, 2)? as u32),
        _ => None,
    }
}

struct Walker<'a> {
    code: &'a Code,
    dol: &'a Dol,
    cfg: &'a AnalysisConfig,
    starts: &'a BTreeMap<u32, Origin>,
    covered: Vec<u32>,
    functions: Vec<Function>,
    new_starts: Vec<(u32, Origin)>,
    tables: Vec<JumpTable>,
    indirect: Vec<IndirectBranch>,
    warnings: BTreeSet<String>,
    // current function
    start: u32,
    next_start: Option<u32>,
}

impl Walker<'_> {
    fn internal(&self, t: u32) -> bool {
        t >= self.start && self.next_start.is_none_or(|n| t < n)
    }

    fn call(&mut self, at: u32, t: Option<u32>) {
        match t {
            Some(t) if self.code.valid(t) => self.new_starts.push((t, Origin::Call)),
            Some(t) => {
                self.warnings.insert(format!("{at:#010x}: call target {t:#010x} is not decodable code"));
            }
            None => {}
        }
    }

    /// Classify a branch target: internal (index to continue at) or a tail call.
    fn jump(&mut self, at: u32, t: Option<u32>) -> Option<usize> {
        let t = t?;
        let Some(i) = self.code.idx(t) else {
            self.warnings.insert(format!("{at:#010x}: branch target {t:#010x} is outside analysed code"));
            return None;
        };
        if self.internal(t) {
            Some(i)
        } else {
            if self.code.valid(t) {
                self.new_starts.push((t, Origin::TailCall));
            }
            None
        }
    }

    fn walk(&mut self, s: u32, seen: &mut [u32], stamp: u32, origin: Origin) {
        let Some(sidx) = self.code.idx(s) else {
            self.warnings.insert(format!("{s:#010x}: function start is not analysed code"));
            return;
        };
        self.start = s;
        self.next_start = self.starts.range(s + 1..).next().map(|(&a, _)| a);
        let mut stack = vec![sidx];
        let mut maxi = sidx;
        while let Some(mut i) = stack.pop() {
            loop {
                let a = self.code.addr[i];
                if a != s && self.starts.contains_key(&a) {
                    self.warnings.insert(format!("{a:#010x}: function {s:#010x} runs into start {a:#010x} (false start, or call to a noreturn function)"));
                    break;
                }
                if seen[i] == stamp {
                    break;
                }
                seen[i] = stamp;
                if self.covered[i] == 0 {
                    self.covered[i] = s;
                }
                maxi = maxi.max(i);
                let ins = self.code.ins[i];
                let always = ins.field_bo() & 0x14 == 0x14;
                let lk = ins.field_lk();
                let mut falls = true;
                match ins.op {
                    Opcode::Rfi => falls = false,
                    Opcode::Illegal => {
                        self.warnings.insert(format!("{a:#010x}: illegal instruction {:#010x} in function {s:#010x}", ins.code));
                        falls = false;
                    }
                    Opcode::B => {
                        let t = ins.branch_dest(a);
                        if lk {
                            self.call(a, t);
                        } else {
                            stack.extend(self.jump(a, t));
                            falls = false;
                        }
                    }
                    Opcode::Bc => {
                        let t = ins.branch_dest(a);
                        if lk {
                            // `bcl 20,31,+4` only reads the PC.
                            if t != Some(a + 4) {
                                self.call(a, t);
                            }
                        } else {
                            stack.extend(self.jump(a, t));
                            falls = !always;
                        }
                    }
                    Opcode::Bclr => {
                        if lk {
                            self.indirect.push(IndirectBranch { addr: a, function: s, kind: IndirectKind::CallLr });
                        } else {
                            falls = !always;
                        }
                    }
                    Opcode::Bcctr => {
                        if lk {
                            self.indirect.push(IndirectBranch { addr: a, function: s, kind: IndirectKind::CallCtr });
                        } else {
                            match resolve_table(self.code, self.dol, self.cfg, sidx, s, s, i) {
                                Some(t) if always => {
                                    for &target in &t.targets {
                                        if let Some(ti) = self.code.idx(target) {
                                            stack.push(ti);
                                        }
                                    }
                                    self.tables.push(t);
                                }
                                _ => self.indirect.push(IndirectBranch { addr: a, function: s, kind: IndirectKind::UnresolvedJump }),
                            }
                            falls = !always;
                        }
                    }
                    _ => {}
                }
                if !falls {
                    break;
                }
                match self.code.next(i) {
                    Some(n) => i = n,
                    None => break,
                }
            }
        }
        self.functions.push(Function { start: s, end: self.code.addr[maxi] + 4, origin });
    }
}

/// Resolve the jump table feeding the `bctr` at index `i`: config override first, then the
/// CodeWarrior pattern `cmplwi; bgt; lis/addi; slwi; lwzx; mtctr; bctr`.
/// `lo_limit` bounds how far back to look; targets must be valid code at or above `s_min`.
fn resolve_table(code: &Code, dol: &Dol, cfg: &AnalysisConfig, lo_limit: usize, s_min: u32, function: u32, i: usize) -> Option<JumpTable> {
    let valid_target = |t: u32| t >= s_min && code.valid(t);
    let a = code.addr[i];
    if let Some(o) = cfg.jump_tables.iter().find(|o| o.branch == a) {
        let targets: Vec<u32> = if !o.targets.is_empty() {
            o.targets.clone()
        } else {
            let (table, count) = (o.table?, o.count?);
            (0..count).map(|k| dol.read_u32(table + 4 * k)).collect::<Option<_>>()?
        };
        return Some(JumpTable { branch: a, function, table: o.table, targets, source: TableSource::Config });
    }

    let lo = lo_limit.max(i.saturating_sub(64));
    let mt = (lo..i).rev().find(|&j| {
        let p = code.ins[j].basic();
        code.ins[j].op == Opcode::Mtspr && matches!(p.args[0], Argument::SPR(SPR(9)))
    })?;
    let rc = gpr(&code.ins[mt].basic(), 1)?;
    let d = (lo..mt).rev().find(|&j| defines(code.ins[j], rc))?;
    let p = code.ins[d].basic();
    let base = match code.ins[d].op {
        Opcode::Lwzx => [1, 2].into_iter().find_map(|k| const_val(code, d, lo, gpr(&p, k)?, 6)),
        Opcode::Lwz => {
            let off = offset(&p, 1)? as i32 as u32;
            let rt = gpr(&p, 2)?;
            match const_val(code, d, lo, rt, 6) {
                Some(c) => Some(c.wrapping_add(off)),
                None => {
                    let j = (lo..d).rev().find(|&j| defines(code.ins[j], rt))?;
                    if code.ins[j].op != Opcode::Add {
                        return None;
                    }
                    let q = code.ins[j].basic();
                    [1, 2]
                        .into_iter()
                        .find_map(|k| const_val(code, j, lo, gpr(&q, k)?, 6))
                        .map(|c| c.wrapping_add(off))
                }
            }
        }
        _ => None,
    }?;

    let bound = (lo..d).rev().find_map(|j| code.ins[j].ins_basic_cmpli());
    let (targets, source) = match bound {
        Some(imm) => {
            let t: Option<Vec<u32>> = (0..imm as u32 + 1)
                .map(|k| dol.read_u32(base + 4 * k).filter(|&t| valid_target(t)))
                .collect();
            (t?, TableSource::Auto)
        }
        None => {
            let t: Vec<u32> = (0..1024u32)
                .map_while(|k| dol.read_u32(base + 4 * k).filter(|&t| valid_target(t)))
                .collect();
            if t.len() < 2 {
                return None;
            }
            (t, TableSource::AutoUnbounded)
        }
    };
    Some(JumpTable { branch: a, function, table: Some(base), targets, source })
}
trait CmpliBound {
    /// Immediate of a `cmplwi`, if this is one.
    fn ins_basic_cmpli(&self) -> Option<u16>;
}

impl CmpliBound for Ins {
    fn ins_basic_cmpli(&self) -> Option<u16> {
        (self.op == Opcode::Cmpli).then(|| uimm(&self.basic(), 3)).flatten()
    }
}

struct Pass {
    covered: Vec<u32>,
    functions: Vec<Function>,
    new_starts: Vec<(u32, Origin)>,
    tables: Vec<JumpTable>,
    indirect: Vec<IndirectBranch>,
    warnings: BTreeSet<String>,
}

fn walk_all(code: &Code, dol: &Dol, cfg: &AnalysisConfig, starts: &BTreeMap<u32, Origin>) -> Pass {
    let mut w = Walker {
        code,
        dol,
        cfg,
        starts,
        covered: vec![0; code.ins.len()],
        functions: vec![],
        new_starts: vec![],
        tables: vec![],
        indirect: vec![],
        warnings: BTreeSet::new(),
        start: 0,
        next_start: None,
    };
    let mut seen = vec![0u32; code.ins.len()];
    for (ord, (&s, &origin)) in starts.iter().enumerate() {
        w.walk(s, &mut seen, ord as u32 + 1, origin);
    }
    Pass {
        covered: w.covered,
        functions: w.functions,
        new_starts: w.new_starts,
        tables: w.tables,
        indirect: w.indirect,
        warnings: w.warnings,
    }
}

/// Pointer-like constants: words in data sections and `lis`+`addi`/`ori` pairs that name code.
fn candidates(code: &Code, dol: &Dol, skip: &[(u32, u32)]) -> Vec<(u32, Origin)> {
    let mut out = Vec::new();
    for s in dol.data_sections() {
        for (k, w) in dol.section_bytes(s).chunks_exact(4).enumerate() {
            let a = s.addr + 4 * k as u32;
            if skip.iter().any(|&(b, e)| a >= b && a < e) {
                continue;
            }
            let v = u32::from_be_bytes(w.try_into().unwrap());
            if code.valid(v) {
                out.push((v, Origin::DataPointer));
            }
        }
    }
    for i in 0..code.ins.len() {
        if code.ins[i].op != Opcode::Addis {
            continue;
        }
        let p = code.ins[i].basic();
        let (Some(rd), Some(0), Some(hi)) = (gpr(&p, 0), gpr(&p, 1), uimm(&p, 2)) else { continue };
        let hi = (hi as u32) << 16;
        let mut j = i;
        for _ in 0..5 {
            let Some(n) = code.next(j) else { break };
            j = n;
            let q = code.ins[j].basic();
            let v = match code.ins[j].op {
                Opcode::Addi if gpr(&q, 1) == Some(rd) => simm(&q, 2).map(|l| hi.wrapping_add(l as i32 as u32)),
                Opcode::Ori if gpr(&q, 1) == Some(rd) => uimm(&q, 2).map(|l| hi | l as u32),
                _ => None,
            };
            if let Some(v) = v.filter(|&v| code.valid(v)) {
                out.push((v, Origin::AddressLoad));
            }
        }
    }
    out
}

/// Runs of allowed, uncovered, non-zero words. Zero words are alignment padding.
fn uncovered_runs(code: &Code, covered: &[u32]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut cur: Option<usize> = None;
    for i in 0..code.ins.len() {
        let gap = code.allowed[i] && covered[i] == 0 && code.ins[i].code != 0;
        let contiguous = cur.is_some() && i > 0 && code.addr[i] == code.addr[i - 1] + 4;
        match (gap, cur) {
            (true, None) => cur = Some(i),
            (true, Some(_)) if !contiguous => {
                runs.push((cur.unwrap(), i));
                cur = Some(i);
            }
            (false, Some(s)) => {
                runs.push((s, i));
                cur = None;
            }
            _ => {}
        }
    }
    if let Some(s) = cur {
        runs.push((s, code.ins.len()));
    }
    runs
}

/// Whether executing `ins` can continue at the next instruction in a way that proves the
/// next address is not a function start. Calls are ambiguous (noreturn callees) and say no.
fn proves_fallthrough(ins: Ins) -> bool {
    let always = ins.field_bo() & 0x14 == 0x14;
    match ins.op {
        Opcode::B | Opcode::Rfi | Opcode::Illegal => false,
        Opcode::Bc | Opcode::Bclr | Opcode::Bcctr => !ins.field_lk() && !always,
        _ => true,
    }
}

struct Discovery {
    pass: Pass,
    starts: BTreeMap<u32, Origin>,
}

/// Grow the set of starts to a fixpoint. `reject` holds addresses that must not become starts.
fn discover(code: &Code, dol: &Dol, cfg: &AnalysisConfig, table_ranges: &[(u32, u32)], reject: &BTreeSet<u32>) -> Discovery {
    let mut starts: BTreeMap<u32, Origin> = BTreeMap::new();
    let add = |starts: &mut BTreeMap<u32, Origin>, a: u32, o: Origin| -> bool {
        !reject.contains(&a) && code.valid(a) && starts.insert(a, o).is_none()
    };
    add(&mut starts, dol.entry, Origin::Entry);
    for &f in &cfg.functions {
        add(&mut starts, f, Origin::Config);
    }

    let pass = loop {
        let pass = walk_all(code, dol, cfg, &starts);
        let mut changed = false;
        for &(a, o) in &pass.new_starts {
            changed |= add(&mut starts, a, o);
        }
        if changed {
            continue;
        }

        for (a, o) in candidates(code, dol, table_ranges) {
            if code.idx(a).is_some_and(|i| pass.covered[i] == 0) {
                changed |= add(&mut starts, a, o);
            }
        }
        if changed {
            continue;
        }

        for (s, _) in uncovered_runs(code, &pass.covered) {
            changed |= add(&mut starts, code.addr[s], Origin::Gap);
        }
        if !changed {
            break pass;
        }
    };
    Discovery { pass, starts }
}

pub fn analyze(dol: &Dol, cfg: &AnalysisConfig) -> Analysis {
    let code = Code::build(dol, cfg);
    let mut reject: BTreeSet<u32> = cfg.not_functions.iter().copied().collect();
    let mut warnings = BTreeSet::new();
    if !code.valid(dol.entry) {
        warnings.insert(format!("entry point {:#010x} is not analysed code", dol.entry));
    }
    for &f in &cfg.functions {
        if !code.valid(f) {
            warnings.insert(format!("configured function {f:#010x} is not valid code"));
        }
    }

    // Jump tables of functions not discovered yet must not leak case labels into the pointer candidates.
    let mut table_ranges: Vec<(u32, u32)> = Vec::new();
    for i in 0..code.ins.len() {
        let ins = code.ins[i];
        if ins.op == Opcode::Bcctr && !ins.field_lk() && ins.field_bo() & 0x14 == 0x14 {
            let a = code.addr[i];
            if let Some(t) = resolve_table(&code, dol, cfg, 0, a.saturating_sub(0x8000), 0, i) {
                if let Some(b) = t.table {
                    table_ranges.push((b, b + 4 * t.targets.len() as u32));
                }
            }
        }
    }

    // Untrusted starts that preceding code runs straight into are mid-function labels: reject and redo.
    let mut rejected: Vec<(u32, Origin)> = Vec::new();
    let Discovery { pass, starts: _ } = loop {
        let d = discover(&code, dol, cfg, &table_ranges, &reject);
        let bad: Vec<(u32, Origin)> = d
            .starts
            .iter()
            .filter(|&(_, &o)| matches!(o, Origin::TailCall | Origin::DataPointer | Origin::AddressLoad | Origin::Gap))
            .filter(|&(&a, _)| {
                code.idx(a).is_some_and(|i| {
                    i > 0 && code.addr[i - 1] + 4 == a && d.pass.covered[i - 1] != 0 && proves_fallthrough(code.ins[i - 1])
                })
            })
            .map(|(&a, &o)| (a, o))
            .collect();
        if bad.is_empty() {
            break d;
        }
        for (a, o) in bad {
            reject.insert(a);
            rejected.push((a, o));
        }
    };

    let mut a = Analysis { warnings: Vec::new(), rejected, ..Analysis::default() };
    warnings.extend(pass.warnings);
    a.warnings = warnings.into_iter().collect();
    let mut functions = pass.functions;
    functions.sort_by_key(|f| f.start);
    a.functions = functions;
    a.jump_tables = pass.tables;
    a.jump_tables.sort_by_key(|t| t.branch);
    a.indirect = pass.indirect;
    a.indirect.sort_by_key(|b| b.addr);
    a.text_bytes = code.ins.len() as u32 * 4;
    a.covered_bytes = pass.covered.iter().filter(|&&c| c != 0).count() as u32 * 4;
    for (s, e) in uncovered_runs(&code, &pass.covered) {
        a.unreached.push(Range { start: code.addr[s], end: code.addr[e - 1] + 4 });
    }
    a
}

impl Analysis {
    pub fn count_origin(&self, o: Origin) -> usize {
        self.functions.iter().filter(|f| f.origin == o).count()
    }

    pub fn summary(&self) -> String {
        let n = |k: IndirectKind| self.indirect.iter().filter(|b| b.kind == k).count();
        let origins = [
            ("entry", Origin::Entry),
            ("config", Origin::Config),
            ("call", Origin::Call),
            ("tail call", Origin::TailCall),
            ("data pointer", Origin::DataPointer),
            ("address load", Origin::AddressLoad),
            ("gap", Origin::Gap),
        ]
        .map(|(l, o)| format!("{l} {}", self.count_origin(o)))
        .join(", ");
        format!(
            "{} functions ({origins})\n{} of {} text bytes covered, {} unreached ranges\n{} jump tables resolved ({} bounded, {} unbounded)\n{} unresolved bctr, {} bctrl, {} blrl\n{} starts rejected as mid-function labels, {} warnings",
            self.functions.len(),
            self.covered_bytes,
            self.text_bytes,
            self.unreached.len(),
            self.jump_tables.len(),
            self.jump_tables.iter().filter(|t| t.source != TableSource::AutoUnbounded).count(),
            self.jump_tables.iter().filter(|t| t.source == TableSource::AutoUnbounded).count(),
            n(IndirectKind::UnresolvedJump),
            n(IndirectKind::CallCtr),
            n(IndirectKind::CallLr),
            self.rejected.len(),
            self.warnings.len(),
        )
    }

    /// Full human-readable report (greppable, one item per line).
    pub fn report(&self) -> String {
        use std::fmt::Write;
        let mut s = String::new();
        let _ = writeln!(s, "# Summary\n{}\n", self.summary());
        let _ = writeln!(s, "# Unresolved indirect branches (runtime dispatch or config needed)");
        for b in self.indirect.iter().filter(|b| b.kind == IndirectKind::UnresolvedJump) {
            let _ = writeln!(s, "bctr {:#010x} in function {:#010x}", b.addr, b.function);
        }
        let _ = writeln!(s, "\n# Indirect calls (runtime dispatch)");
        for b in self.indirect.iter().filter(|b| b.kind != IndirectKind::UnresolvedJump) {
            let kind = if b.kind == IndirectKind::CallCtr { "bctrl" } else { "blrl" };
            let _ = writeln!(s, "{kind} {:#010x} in function {:#010x}", b.addr, b.function);
        }
        let _ = writeln!(s, "\n# Jump tables");
        for t in &self.jump_tables {
            let table = t.table.map_or("-".into(), |a| format!("{a:#010x}"));
            let _ = writeln!(s, "bctr {:#010x} table {table} entries {} {:?}", t.branch, t.targets.len(), t.source);
        }
        let _ = writeln!(s, "\n# Unreached code");
        for r in &self.unreached {
            let _ = writeln!(s, "{:#010x}-{:#010x} ({} bytes)", r.start, r.end, r.end - r.start);
        }
        let _ = writeln!(s, "\n# Rejected starts (mid-function labels)");
        for (a, o) in &self.rejected {
            let _ = writeln!(s, "{a:#010x} {o:?}");
        }
        let _ = writeln!(s, "\n# Warnings");
        for w in &self.warnings {
            let _ = writeln!(s, "{w}");
        }
        let _ = writeln!(s, "\n# Functions");
        for f in &self.functions {
            let _ = writeln!(s, "{:#010x}-{:#010x} {:?}", f.start, f.end, f.origin);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: u32 = 0x8000_3100;
    const DATA: u32 = 0x8001_0000;
    const BLR: u32 = 0x4E80_0020;
    const BCTR: u32 = 0x4E80_0420;

    fn li(rd: u32, imm: i16) -> u32 {
        0x3800_0000 | rd << 21 | (imm as u16 as u32)
    }
    fn lis(rd: u32, imm: u32) -> u32 {
        0x3C00_0000 | rd << 21 | imm
    }
    fn addi(rd: u32, ra: u32, imm: u32) -> u32 {
        0x3800_0000 | rd << 21 | ra << 16 | (imm & 0xFFFF)
    }
    fn b(off: i32) -> u32 {
        0x4800_0000 | (off as u32 & 0x03FF_FFFC)
    }
    fn bl(off: i32) -> u32 {
        b(off) | 1
    }
    fn bgt(off: i32) -> u32 {
        0x4181_0000 | (off as u32 & 0xFFFC)
    }
    fn beq(off: i32) -> u32 {
        0x4182_0000 | (off as u32 & 0xFFFC)
    }
    fn cmplwi(ra: u32, imm: u32) -> u32 {
        0x2800_0000 | ra << 16 | imm
    }
    const SLWI_R0_R3_2: u32 = 0x5460_103A;
    fn lwzx(rd: u32, ra: u32, rb: u32) -> u32 {
        0x7C00_002E | rd << 21 | ra << 16 | rb << 11
    }
    const MTCTR_R0: u32 = 0x7C09_03A6;

    fn dol(text: &[u32], data: &[u32]) -> Dol {
        let mut h = vec![0u8; 0x100];
        let tlen = text.len() as u32 * 4;
        let put = |h: &mut Vec<u8>, off: usize, v: u32| h[off..off + 4].copy_from_slice(&v.to_be_bytes());
        put(&mut h, 0, 0x100);
        put(&mut h, 0x48, TEXT);
        put(&mut h, 0x90, tlen);
        if !data.is_empty() {
            put(&mut h, 7 * 4, 0x100 + tlen);
            put(&mut h, 0x48 + 7 * 4, DATA);
            put(&mut h, 0x90 + 7 * 4, data.len() as u32 * 4);
        }
        put(&mut h, 0xE0, TEXT);
        for w in text.iter().chain(data) {
            h.extend_from_slice(&w.to_be_bytes());
        }
        Dol::parse(h).unwrap()
    }

    fn starts(a: &Analysis) -> Vec<(u32, Origin)> {
        a.functions.iter().map(|f| (f.start - TEXT, f.origin)).collect()
    }

    #[test]
    fn calls_tail_calls_and_internal_branches() {
        let text = [
            bl(0x10),    // 0x00 -> f1
            b(0x14),     // 0x04 tail call -> f2
            0,           // 0x08 padding
            0,           // 0x0c padding
            li(3, 1),    // 0x10 f1
            BLR,         // 0x14
            li(3, 2),    // 0x18 f2 (0x04 + 0x14)
            beq(0x8),    // 0x1c forward, internal
            li(3, 3),    // 0x20
            BLR,         // 0x24
        ];
        // f1 at 0x10 (bl 0x00+0x10), f2 at 0x18 (b 0x04+0x14)
        let a = analyze(&dol(&text, &[]), &AnalysisConfig::default());
        assert_eq!(starts(&a), [(0, Origin::Entry), (0x10, Origin::Call), (0x18, Origin::TailCall)]);
        let f2 = &a.functions[2];
        assert_eq!(f2.end - TEXT, 0x28);
        assert!(a.unreached.is_empty());
        assert!(a.indirect.is_empty());
    }

    #[test]
    fn jump_table_is_resolved_and_targets_stay_internal() {
        let t = |off: u32| TEXT + off;
        let text = [
            cmplwi(3, 2),                      // 0x00
            bgt(0x20),                         // 0x04 -> default 0x24
            lis(4, DATA >> 16),                // 0x08
            addi(4, 4, DATA & 0xFFFF),         // 0x0c
            SLWI_R0_R3_2,                      // 0x10
            lwzx(0, 4, 0),                     // 0x14
            MTCTR_R0,                          // 0x18
            BCTR,                              // 0x1c
            li(3, 1),                          // 0x20 case 0
            BLR,                               // 0x24
            li(3, 2),                          // 0x28 case 1
            BLR,                               // 0x2c
            li(3, 3),                          // 0x30 case 2
            BLR,                               // 0x34
        ];
        let data = [t(0x20), t(0x28), t(0x30)];
        let a = analyze(&dol(&text, &data), &AnalysisConfig::default());
        assert_eq!(a.functions.len(), 1, "{:?}", a.functions);
        assert_eq!(a.functions[0].end - TEXT, 0x38);
        assert_eq!(a.jump_tables.len(), 1);
        assert_eq!(a.jump_tables[0].targets, data);
        assert_eq!(a.jump_tables[0].source, TableSource::Auto);
        assert!(a.indirect.is_empty());
    }

    #[test]
    fn unreferenced_code_becomes_a_gap_function_and_data_pointers_are_found() {
        let text = [
            BLR,      // 0x00 entry
            li(3, 1), // 0x04 gap function
            BLR,      // 0x08
            li(3, 2), // 0x0c referenced only from data
            BLR,      // 0x10
        ];
        let data = [TEXT + 0xC];
        let a = analyze(&dol(&text, &data), &AnalysisConfig::default());
        assert_eq!(
            starts(&a),
            [(0, Origin::Entry), (4, Origin::Gap), (0xC, Origin::DataPointer)]
        );
    }

    #[test]
    fn config_overrides_apply() {
        let text = [BLR, li(3, 1), BLR, li(3, 2), BLR];
        let cfg = AnalysisConfig { not_functions: vec![TEXT + 4], functions: vec![TEXT + 0xC], ..Default::default() };
        let a = analyze(&dol(&text, &[]), &cfg);
        // 0x04 is rejected as a start, so the gap run is left unreached; 0x0c is forced.
        assert_eq!(starts(&a), [(0, Origin::Entry), (0xC, Origin::Config)]);
        assert_eq!(a.unreached, [Range { start: TEXT + 4, end: TEXT + 0xC }]);

        let text = [b(0x8), 0, BLR, BCTR];
        let a = analyze(&dol(&text, &[]), &AnalysisConfig::default());
        assert_eq!(a.indirect.iter().filter(|i| i.kind == IndirectKind::UnresolvedJump).count(), 1);
    }
}
