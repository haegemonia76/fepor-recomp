//! Native code backend (ticket M7.0, ADR 0011 and 0024): the lifter's job done in-process with Cranelift.
//!
//! [`Jit`] holds the program (the DOL and its analysis) and compiles guest functions to native code on
//! first use. A compiled function is an `extern "C" fn(*mut Context)` like the generated C++ ones, so the
//! runtime cannot tell the two backends apart. Guest calls go through a table of function pointers whose
//! empty slots compile the callee when first called.

mod fp;
mod translate;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};

use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context as ClifContext;
use cranelift_frontend::FunctionBuilderContext;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, FuncId, Module};
use fepor_core::analysis::{Analysis, Function, JumpTable};
use fepor_core::config::HleEntry;
use fepor_core::dol::Dol;
use fepor_runtime::Context;

pub type HostFn = unsafe extern "C" fn(*mut Context);

/// Functions a call to which the runtime implements (`fepor_hle_<name>`): name to address.
pub type HleSymbols = HashMap<String, usize>;

struct State {
    module: JITModule,
    clif: ClifContext,
    fbc: FunctionBuilderContext,
    imports: HashMap<String, FuncId>,
    counter: u32,
    compiled: u32,
    compile_ms: f64,
}

/// The one compiler of the process: JIT code holds its address, so it lives in a static.
static JIT: OnceLock<Jit> = OnceLock::new();

/// The compiler and the code it produced.
pub struct Jit {
    dol: Dol,
    functions: Vec<Function>,
    index: HashMap<u32, usize>,
    tables: HashMap<u32, JumpTable>,
    hle: HashMap<u32, HleEntry>,
    /// Addresses after the first call of a function that saves a context (see `HleEntry::resumes_at`).
    save_targets: BTreeSet<u32>,
    table: Box<[AtomicPtr<u8>]>,
    trace_calls: bool,
    /// `FEPOR_TRACE=jit`: how long compiling takes.
    trace_jit: bool,
    state: Mutex<State>,
}

// SAFETY: the code pointers are immutable once published and the compiler state is behind a mutex.
unsafe impl Send for Jit {}
unsafe impl Sync for Jit {}

impl Jit {
    /// Set up the compiler for this program (once per process; later calls return the first one).
    /// `hle_symbols` maps the runtime-implemented SDK functions (`HleEntry::name`) to their addresses.
    pub fn install(
        dol: Dol,
        analysis: &Analysis,
        hle: &[HleEntry],
        hle_symbols: &HleSymbols,
        trace_calls: bool,
    ) -> &'static Jit {
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").expect("opt_level");
        // JIT code and runtime are one image: no PLT/GOT indirection needed
        flags.set("is_pic", "false").expect("is_pic");
        let isa = cranelift_native::builder()
            .expect("host ISA")
            .finish(settings::Flags::new(flags))
            .expect("ISA");
        let mut jb = JITBuilder::with_isa(isa, default_libcall_names());
        translate::register_symbols(&mut jb, hle_symbols);
        let module = JITModule::new(jb);
        let functions = analysis.functions.clone();
        let index: HashMap<u32, usize> = functions
            .iter()
            .enumerate()
            .map(|(i, f)| (f.start, i))
            .collect();
        let tables = analysis
            .jump_tables
            .iter()
            .map(|t| (t.branch, t.clone()))
            .collect();
        let hle_map: HashMap<u32, HleEntry> = hle.iter().map(|h| (h.addr, h.clone())).collect();
        let save_targets = hle.iter().filter_map(|h| h.resumes_at).collect();
        let table = (0..functions.len())
            .map(|_| AtomicPtr::new(std::ptr::null_mut()))
            .collect();
        JIT.get_or_init(|| Jit {
            dol,
            functions,
            index,
            tables,
            hle: hle_map,
            save_targets,
            table,
            trace_calls,
            trace_jit: std::env::var("FEPOR_TRACE").is_ok_and(|v| v.split(',').any(|x| x == "jit")),
            state: Mutex::new(State {
                clif: module.make_context(),
                module,
                fbc: FunctionBuilderContext::new(),
                imports: HashMap::new(),
                counter: 0,
                compiled: 0,
                compile_ms: 0.0,
            }),
        })
    }

    /// The compiler of this process, once `install` has run.
    pub fn global() -> Option<&'static Jit> {
        JIT.get()
    }

    /// The host function for guest address `target`, compiled now if need be.
    pub fn entry(&self, target: u32) -> Option<HostFn> {
        let i = *self.index.get(&target)?;
        // SAFETY: the pointer is either a finished function of the right signature or null (compile below).
        Some(unsafe { std::mem::transmute::<*mut u8, HostFn>(self.resolve(i)) })
    }

    /// Compile function `i` unless already done; the code address.
    fn resolve(&self, i: usize) -> *mut u8 {
        let p = self.table[i].load(Ordering::Acquire);
        if !p.is_null() {
            return p;
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let p = self.table[i].load(Ordering::Acquire);
        if !p.is_null() {
            return p;
        }
        let started = std::time::Instant::now();
        let p = translate::compile(self, &mut st, i);
        self.table[i].store(p, Ordering::Release);
        if self.trace_jit {
            let ms = started.elapsed().as_secs_f64() * 1e3;
            st.compiled += 1;
            st.compile_ms += ms;
            if st.compiled % 100 == 0 {
                eprintln!(
                    "jit: {} functions compiled in {:.0} ms ({:.2} ms each); last {:08x}, {} instructions, {ms:.2} ms",
                    st.compiled,
                    st.compile_ms,
                    st.compile_ms / st.compiled as f64,
                    self.functions[i].start,
                    (self.functions[i].end - self.functions[i].start) / 4
                );
            }
        }
        p
    }
}
