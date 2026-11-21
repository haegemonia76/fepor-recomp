use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io;

use fepor_core::analysis::{Analysis, Function, JumpTable};
use fepor_core::config::HleEntry;
use fepor_core::dol::Dol;
use ppc750cl::{Ins, Opcode};

use crate::ins::{fname, Fx, Used};

pub struct Options {
    /// Functions per generated compilation unit.
    pub functions_per_unit: usize,
    /// Path of the runtime's CMake project, written into the generated CMakeLists.txt.
    pub runtime_dir: String,
    /// Emit a `fepor_enter(addr)` call at every function entry (call tracing).
    pub trace_calls: bool,
    /// Guest functions replaced by runtime implementations.
    pub hle: Vec<HleEntry>,
}

#[derive(Debug, Default)]
pub struct Stats {
    pub functions: usize,
    pub instructions: usize,
    pub units: usize,
    /// Instructions emitted as runtime traps, by opcode.
    pub unimplemented: BTreeMap<String, usize>,
    pub unimplemented_sites: Vec<(u32, u32)>,
}

#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("function {0:#010x} reads outside the DOL image")]
    OutsideImage(u32),
    #[error("HLE entry {name} at {addr:#010x} is not a function start")]
    HleNotAFunction { addr: u32, name: String },
}

fn sync_lines(used: &Used, to_ctx: bool) -> String {
    let mut items: Vec<(String, String)> = Vec::new();
    for n in 0..32 {
        if used.gpr >> n & 1 != 0 {
            items.push((format!("r[{n}]"), format!("c.r[{n}]")));
        }
        if used.fpr >> n & 1 != 0 {
            items.push((format!("f[{n}]"), format!("c.f[{n}]")));
        }
    }
    for (on, name) in [
        (used.lr, "lr"),
        (used.ctr, "ctr"),
        (used.xer, "xer"),
        (used.fpscr, "fpscr"),
    ] {
        if on {
            items.push((name.into(), format!("c.{name}")));
        }
    }
    let mut s = String::new();
    for (local, ctx) in items {
        let _ = if to_ctx {
            write!(s, " {ctx} = {local};")
        } else {
            write!(s, " {local} = {ctx};")
        };
    }
    if used.cr {
        s.push_str(if to_ctx {
            " for (int i = 0; i < 8; i++) c.cr[i] = cr[i];"
        } else {
            " for (int i = 0; i < 8; i++) cr[i] = c.cr[i];"
        });
    }
    s
}

fn lift_function(
    opts: &Options,
    dol: &Dol,
    f: &Function,
    funcs: &BTreeSet<u32>,
    tables: &HashMap<u32, &JumpTable>,
    hle: &HashMap<u32, &HleEntry>,
    stats: &mut Stats,
) -> Result<String, GenerateError> {
    if let Some(h) = hle.get(&f.start) {
        return Ok(format!(
            "void {}(Context& __restrict c) {{\n    fepor_hle_{}(&c);\n}}\n\n",
            fname(f.start),
            h.name
        ));
    }
    let words: Vec<(u32, Ins)> = (f.start..f.end)
        .step_by(4)
        .map(|a| {
            dol.read_u32(a)
                .map(|w| (a, Ins::new(w)))
                .ok_or(GenerateError::OutsideImage(f.start))
        })
        .collect::<Result<_, _>>()?;

    let mut labels = BTreeSet::new();
    let internal = |t: u32| t >= f.start && t < f.end;
    for &(a, ins) in &words {
        match ins.op {
            Opcode::B | Opcode::Bc if !ins.field_lk() => {
                labels.extend(ins.branch_dest(a).filter(|&t| internal(t)))
            }
            Opcode::Bcctr if !ins.field_lk() => {
                if let Some(t) = tables.get(&a) {
                    labels.extend(t.targets.iter().copied().filter(|&t| internal(t)));
                }
            }
            _ => {}
        }
    }

    // HLE "save" functions: remember where the first call returns, so calls to a resuming HLE function
    // (OSLoadContext) can continue there when the thread is resumed.
    let save_targets: BTreeSet<u32> = hle.values().filter_map(|h| h.resumes_at).collect();
    let mut resume_labels = BTreeMap::new();
    for &(a, ins) in &words {
        if ins.op == Opcode::B && ins.field_lk() {
            if let Some(t) = ins.branch_dest(a).filter(|t| save_targets.contains(t)) {
                resume_labels.entry(t).or_insert(a + 4);
                labels.insert(a + 4);
            }
        }
    }

    let mut fx = Fx {
        start: f.start,
        end: f.end,
        funcs,
        tables,
        labels,
        used: Used::default(),
        body: Vec::new(),
        unimpl: Vec::new(),
        unimpl_ops: Vec::new(),
        hle,
        resume_labels,
    };
    for &(a, ins) in &words {
        if fx.labels.contains(&a) {
            fx.body.push(format!("L_{a:08x}:;"));
        }
        fx.lift(a, ins);
    }
    // Code runs off the end of the extent: continue in whatever follows.
    let tail = fx_jump_off_end(&mut fx, f.end);
    fx.body.extend(tail);

    stats.instructions += words.len();
    stats.unimplemented_sites.extend(&fx.unimpl);
    for op in &fx.unimpl_ops {
        *stats.unimplemented.entry(format!("{op:?}")).or_default() += 1;
    }

    let u = &fx.used;
    let mut s = String::new();
    let _ = writeln!(s, "void {}(Context& __restrict c) {{", fname(f.start));
    s.push_str("    [[maybe_unused]] uint8_t* const m = c.ram;\n");
    if u.gpr != 0 {
        s.push_str("    uint32_t r[32];\n");
    }
    if u.fpr != 0 {
        s.push_str("    v2d f[32];\n");
    }
    if u.cr {
        s.push_str("    CRF cr[8];\n");
    }
    if u.lr {
        s.push_str("    uint32_t lr;\n");
    }
    if u.ctr {
        s.push_str("    uint32_t ctr;\n");
    }
    if u.xer {
        s.push_str("    Xer xer;\n");
    }
    if u.fpscr {
        s.push_str("    uint32_t fpscr;\n");
    }
    let _ = writeln!(
        s,
        "    [[maybe_unused]] auto in = [&]() {{{} }};",
        sync_lines(u, false)
    );
    let _ = writeln!(
        s,
        "    [[maybe_unused]] auto out = [&]() {{{} }};",
        sync_lines(u, true)
    );
    if opts.trace_calls {
        let _ = writeln!(s, "    fepor_enter(0x{:x});", f.start);
    }
    s.push_str("    in();\n");
    for line in &fx.body {
        s.push_str("    ");
        s.push_str(line);
        s.push('\n');
    }
    s.push_str("}\n\n");
    Ok(s)
}

fn fx_jump_off_end(fx: &mut Fx, end: u32) -> Vec<String> {
    if fx.funcs.contains(&end) {
        vec![
            "out();".into(),
            format!("[[clang::musttail]] return {}(c);", fname(end)),
        ]
    } else {
        vec![
            "out();".into(),
            format!("unresolved(c, 0x{:x}, 0x{end:x});", end - 4),
        ]
    }
}

/// Emit `functions.h`, `unit_NNNN.cpp`, `dispatch.cpp` and `CMakeLists.txt` through `write`.
pub fn generate(
    dol: &Dol,
    analysis: &Analysis,
    opts: &Options,
    write: &mut dyn FnMut(&str, &str) -> io::Result<()>,
) -> Result<Stats, GenerateError> {
    let funcs: BTreeSet<u32> = analysis.functions.iter().map(|f| f.start).collect();
    let tables: HashMap<u32, &JumpTable> =
        analysis.jump_tables.iter().map(|t| (t.branch, t)).collect();
    let hle: HashMap<u32, &HleEntry> = opts.hle.iter().map(|h| (h.addr, h)).collect();
    for h in &opts.hle {
        if !funcs.contains(&h.addr) {
            return Err(GenerateError::HleNotAFunction {
                addr: h.addr,
                name: h.name.clone(),
            });
        }
    }
    let mut stats = Stats {
        functions: analysis.functions.len(),
        ..Stats::default()
    };

    let mut decls = String::from(
        "// Generated by fepor-recomp. Do not edit.\n#pragma once\n#include \"fepor/ppc.h\"\n\n",
    );
    for f in &analysis.functions {
        let _ = writeln!(decls, "void {}(fepor::Context&);", fname(f.start));
    }
    for h in &opts.hle {
        let _ = writeln!(
            decls,
            "extern \"C\" void fepor_hle_{}(fepor::Context*);",
            h.name
        );
    }
    write("functions.h", &decls)?;

    let per_unit = opts.functions_per_unit.max(1);
    let mut units = Vec::new();
    for (i, chunk) in analysis.functions.chunks(per_unit).enumerate() {
        let mut s = String::from("// Generated by fepor-recomp. Do not edit.\n#include \"functions.h\"\n\nusing namespace fepor;\n\n");
        for f in chunk {
            s.push_str(&lift_function(
                opts, dol, f, &funcs, &tables, &hle, &mut stats,
            )?);
        }
        let name = format!("unit_{i:04}.cpp");
        write(&name, &s)?;
        units.push(name);
    }
    stats.units = units.len();

    let mut d = String::from("// Generated by fepor-recomp. Do not edit.\n#include \"fepor/dispatch.h\"\n#include \"functions.h\"\n\nconst fepor::DispatchEntry fepor_dispatch_table[] = {\n");
    for f in &analysis.functions {
        let _ = writeln!(d, "    {{0x{:08x}u, {}}},", f.start, fname(f.start));
    }
    let _ = writeln!(
        d,
        "}};\nconst size_t fepor_dispatch_count = {};",
        analysis.functions.len()
    );
    write("dispatch.cpp", &d)?;

    let mut cm = String::from("# Generated by fepor-recomp.\ncmake_minimum_required(VERSION 3.25)\nproject(fepor_generated LANGUAGES CXX)\n\n");
    cm.push_str("set(CMAKE_CXX_STANDARD 20)\nset(CMAKE_CXX_STANDARD_REQUIRED ON)\nset(CMAKE_POSITION_INDEPENDENT_CODE ON)  # linked into position-independent Rust executables\n");
    cm.push_str("if(NOT CMAKE_CXX_COMPILER_ID MATCHES \"Clang\")\n  message(FATAL_ERROR \"Generated code needs clang (ext_vector_type, musttail)\")\nendif()\n\n");
    let _ = writeln!(
        cm,
        "set(FEPOR_RUNTIME_DIR \"{}\" CACHE PATH \"Path to the fepor runtime sources\")",
        opts.runtime_dir.replace('\\', "/")
    );
    cm.push_str("add_subdirectory(${FEPOR_RUNTIME_DIR} fepor_runtime_build)\n\nadd_library(fepor_generated STATIC\n");
    for u in &units {
        let _ = writeln!(cm, "  {u}");
    }
    cm.push_str("  dispatch.cpp)\ntarget_include_directories(fepor_generated PRIVATE ${CMAKE_CURRENT_SOURCE_DIR})\ntarget_link_libraries(fepor_generated PUBLIC fepor_runtime)\ntarget_compile_options(fepor_generated PRIVATE -w)\n");
    write("CMakeLists.txt", &cm)?;
    Ok(stats)
}
