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
    /// Resume variants (`resume_entry`) by function index.
    resumes: Mutex<HashMap<usize, usize>>,
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
            resumes: Mutex::new(HashMap::new()),
            state: Mutex::new(new_state(hle_symbols)),
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

    /// A variant of the function containing guest address `addr` that continues there instead of at its
    /// start: `addr` is the instruction after one of its calls or a loop head, the context holds the
    /// registers, and the caller puts `addr` in `spr[1021]` first (`Context::spr`). This is how a thread
    /// is rebuilt from its guest stack when a save state is loaded (docs/adr/0035). Compiled when first
    /// asked for, not in the table: normal calls never reach it.
    pub fn resume_entry(&self, addr: u32) -> Option<HostFn> {
        let i = self.functions.partition_point(|f| f.start <= addr).checked_sub(1)?;
        let f = &self.functions[i];
        if addr >= f.end || self.hle.contains_key(&f.start) {
            return None;
        }
        if let Some(&p) = self.resumes.lock().unwrap_or_else(|e| e.into_inner()).get(&i) {
            // SAFETY: a finished function of the right signature.
            return Some(unsafe { std::mem::transmute::<usize, HostFn>(p) });
        }
        let p = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            translate::compile(self, &mut st, i, true) as usize
        };
        self.resumes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(i, p);
        // SAFETY: as above.
        Some(unsafe { std::mem::transmute::<usize, HostFn>(p) })
    }

    /// Compile function `i` unless already done; the code address.
    fn resolve(&self, i: usize) -> *mut u8 {
        let p = self.table[i].load(Ordering::Acquire);
        if !p.is_null() {
            return p;
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.compile_with(&mut st, i)
    }

    /// Compile function `i` with compiler `st` unless already done. Two threads may compile the same
    /// function; the first to publish wins and the other's code is simply never used.
    fn compile_with(&self, st: &mut State, i: usize) -> *mut u8 {
        let p = self.table[i].load(Ordering::Acquire);
        if !p.is_null() {
            return p;
        }
        let started = std::time::Instant::now();
        let p = translate::compile(self, st, i, false);
        let p = match self.table[i].compare_exchange(
            std::ptr::null_mut(),
            p,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => p,
            Err(winner) => winner,
        };
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

    /// Compile every function on `workers` background threads, so that the game thread finds its code
    /// ready instead of stalling a few milliseconds on each first call (a frame is 16 ms). Each thread has
    /// its own compiler (modules are not shared); calls between functions go through the table.
    pub fn precompile(&'static self, hle_symbols: HleSymbols, workers: usize) {
        let next = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for w in 0..workers {
            let (syms, next) = (hle_symbols.clone(), next.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("jit-{w}"))
                .spawn(move || {
                    let mut st = new_state(&syms);
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= self.functions.len() {
                            break;
                        }
                        if self.table[i].load(Ordering::Acquire).is_null() {
                            // a function that cannot be compiled is left for the game thread to report
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                self.compile_with(&mut st, i)
                            }));
                        }
                    }
                });
            if let Err(e) = spawned {
                eprintln!("jit: cannot start a compiler thread: {e}");
            }
        }
    }
}

/// A compiler: Cranelift module for the host, with the runtime's symbols.
fn new_state(hle_symbols: &HleSymbols) -> State {
    let mut flags = settings::builder();
    flags.set("opt_level", "speed").expect("opt_level");
    // the translator emits valid IR: the verifier would only cost compile time
    flags
        .set("enable_verifier", "false")
        .expect("enable_verifier");
    // JIT code and runtime are one image: no PLT/GOT indirection needed
    flags.set("is_pic", "false").expect("is_pic");
    let isa = cranelift_native::builder()
        .expect("host ISA")
        .finish(settings::Flags::new(flags))
        .expect("ISA");
    let mut jb = JITBuilder::with_isa(isa, default_libcall_names());
    translate::register_symbols(&mut jb, hle_symbols);
    let module = JITModule::new(jb);
    State {
        clif: module.make_context(),
        module,
        fbc: FunctionBuilderContext::new(),
        imports: HashMap::new(),
        counter: 0,
        compiled: 0,
        compile_ms: 0.0,
    }
}
