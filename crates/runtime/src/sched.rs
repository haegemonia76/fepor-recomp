//! Guest threads. Every guest OSThread runs on its own host thread, but only one runs at a time:
//! a context switch hands the baton over and parks the caller (ADR 0005, amended).
//!
//! A parked thread waits inside the runtime's OSLoadContext. It is identified by the OSContext
//! address it will be resumed from (its token): the context it last saved (resume at the save
//! point) or the context the interrupt handler saved for it (resume where it was interrupted).
//!
//! Host threads are interchangeable workers (`Host`). A guest thread rebuilt from a save state has no
//! host stack: the first switch to it takes an idle host and rebuilds the stack from the guest's own
//! stack (`hle::resume_thread`, ADR 0035). A state load makes every other host unwind its frames and go
//! idle, then installs the loaded threads.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::context::{Context, Fpr};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParkKind {
    /// Resume after the thread's OSSaveContext call (the generated code jumps there).
    AtSave,
    /// Resume at the instruction an interrupt interrupted (the host stack is unwound to the delivery point).
    AfterIrq,
}

/// What an interrupt delivery keeps of the interrupted code on its host stack, so that a thread rebuilt
/// from a save state can do what the unwinding to the delivery point does.
#[derive(Clone)]
pub struct IrqFrame {
    pub f: [Fpr; 32],
    pub fpscr: u32,
    /// Where the interrupted loop continues (`Context::spr[POLL_PC]` at the delivery).
    pub pc: u32,
}

/// A host thread that runs guest threads, one at a time.
pub struct Host {
    cv: Condvar,
    /// A state load asks this (parked) host to abandon its frames.
    killed: AtomicBool,
    /// The guest thread to run next (`NO_JOB` when idle).
    job: AtomicUsize,
}

const NO_JOB: usize = usize::MAX;

impl Host {
    fn new() -> Arc<Host> {
        Arc::new(Host {
            cv: Condvar::new(),
            killed: AtomicBool::new(false),
            job: AtomicUsize::new(NO_JOB),
        })
    }
}

type Body = Box<dyn FnOnce(*mut Context) + Send>;

enum Start {
    /// A thread started by OSLoadContext on a context nobody had run from.
    New(Body),
    /// A thread rebuilt from a save state: the running one, or one parked at its token.
    Resume { running: bool },
}

struct Thread {
    ctx: *mut Context,
    /// Owns `ctx` unless it is the boot thread's.
    _own: Option<Box<Context>>,
    last_save: Option<u32>,
    irq_ctx: Option<u32>,
    irq_frames: Vec<IrqFrame>,
    park: Option<(u32, ParkKind)>,
    host: Option<Arc<Host>>,
    start: Option<Start>,
    /// The body returned: the host parks for good.
    exited: bool,
}

struct State {
    threads: Vec<Thread>,
    running: usize,
    parked: HashMap<u32, (usize, u64)>,
    idle: Vec<Arc<Host>>,
}

// SAFETY: raw context pointers are only dereferenced by the thread holding the baton.
unsafe impl Send for State {}
unsafe impl Send for Thread {}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// Signalled when a host goes idle (a state load waits for the hosts it asked to unwind).
static IDLE_CV: Condvar = Condvar::new();
/// Threads of earlier states, kept until the next load: a host that is still unwinding may touch its context.
static GRAVE: Mutex<Vec<Thread>> = Mutex::new(Vec::new());
static SWITCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

thread_local! {
    static HOST: RefCell<Option<Arc<Host>>> = const { RefCell::new(None) };
    /// This host is abandoning its frames (it was killed, or it loaded a state).
    static UNWINDING: Cell<bool> = const { Cell::new(false) };
}

/// Host thread switches so far (a cost indicator: each one is two futex operations).
pub fn switches() -> u64 {
    SWITCHES.load(Ordering::Relaxed)
}

fn lock() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = lock();
    f(g.as_mut().expect("scheduler not initialised"))
}

fn this_host() -> Arc<Host> {
    HOST.with(|h| h.borrow().clone().expect("not a guest host thread"))
}

/// This host is abandoning its frames: calls into guest code return at once (`hle::run`).
pub fn unwinding() -> bool {
    UNWINDING.with(|u| u.get())
}

pub fn set_unwinding() {
    UNWINDING.with(|u| u.set(true));
}

/// Register the boot thread (thread 0) and make the calling thread its host.
pub fn init(main_ctx: *mut Context) {
    let host = Host::new();
    HOST.with(|h| *h.borrow_mut() = Some(host.clone()));
    let mut g = lock();
    *g = Some(State {
        threads: vec![Thread {
            ctx: main_ctx,
            _own: None,
            last_save: None,
            irq_ctx: None,
            irq_frames: Vec::new(),
            park: None,
            host: Some(host),
            start: None,
            exited: false,
        }],
        running: 0,
        parked: HashMap::new(),
        idle: Vec::new(),
    });
    crate::abi::set_current(main_ctx);
}

pub fn current() -> usize {
    with(|s| s.running)
}

pub fn current_context() -> *mut Context {
    with(|s| s.threads[s.running].ctx)
}

pub fn set_last_save(me: usize, ctx: u32) {
    with(|s| s.threads[me].last_save = Some(ctx));
}

pub fn set_irq_ctx(me: usize, ctx: Option<u32>) {
    with(|s| s.threads[me].irq_ctx = ctx);
}

/// The exception context `me` is currently handling an interrupt for, if any.
pub fn irq_ctx(me: usize) -> Option<u32> {
    with(|s| s.threads[me].irq_ctx)
}

/// An interrupt is being delivered to `me`; `frame` is what the interrupted code leaves behind.
pub fn enter_irq(me: usize, frame: IrqFrame) {
    with(|s| s.threads[me].irq_frames.push(frame));
}

pub fn leave_irq(me: usize) {
    with(|s| {
        s.threads[me].irq_frames.pop();
    });
}

/// The context `me` would be resumed from, and how.
pub fn token(me: usize) -> Option<(u32, ParkKind)> {
    with(|s| {
        let t = &s.threads[me];
        match t.irq_ctx {
            Some(c) => Some((c, ParkKind::AfterIrq)),
            None => t.last_save.map(|c| (c, ParkKind::AtSave)),
        }
    })
}

/// How the thread parked, after it was woken.
pub fn woken_as(me: usize) -> Option<(u32, ParkKind)> {
    with(|s| s.threads[me].park.take())
}

/// The interrupted code's state of thread `me` (while it is inside an interrupt delivery).
pub fn irq_frame(me: usize) -> Option<IrqFrame> {
    with(|s| s.threads[me].irq_frames.last().cloned())
}

/// Which parked thread resumes from this context? A thread that was cancelled and re-created at the same
/// OSThread address leaves its old host thread parked on a context the game has since rewritten
/// (OSCreateThread/OSInitContext): that stale entry is dropped and the load starts a new thread instead.
pub fn parked_at(ctx: u32) -> Option<usize> {
    with(|s| match s.parked.get(&ctx).copied() {
        Some((t, fp)) if fp == fingerprint(ctx) => Some(t),
        Some(_) => {
            s.parked.remove(&ctx);
            None
        }
        None => None,
    })
}

/// The part of an OSContext a parked thread cannot change: PC, stack pointer, LR, CR and r3 as saved.
fn fingerprint(ctx: u32) -> u64 {
    let m = crate::machine::machine();
    [0x198, 0x4, 0x84, 0x80, 0xC]
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, off| {
            (h ^ m.read_u32(ctx + off) as u64).wrapping_mul(0x100_0000_01b3)
        })
}

/// Wait until `me` runs again. False when this host was asked to abandon its frames instead.
fn wait_turn(me: usize) -> bool {
    let host = this_host();
    let mut g = lock();
    loop {
        if host.killed.load(Ordering::Acquire) {
            return false;
        }
        let s = g.as_ref().unwrap();
        if s.running == me {
            break;
        }
        g = host.cv.wait(g).unwrap_or_else(|e| e.into_inner());
    }
    crate::abi::set_current(g.as_ref().unwrap().threads[me].ctx);
    true
}

/// Hand the baton to `other`, giving it a host first when it has none.
fn wake(s: &mut State, other: usize) {
    let host = match s.threads[other].host.clone() {
        Some(h) => h,
        None => {
            let h = match s.idle.pop() {
                Some(h) => h,
                None => {
                    let h = Host::new();
                    spawn_host(h.clone());
                    h
                }
            };
            h.job.store(other, Ordering::Release);
            s.threads[other].host = Some(h.clone());
            h
        }
    };
    s.running = other;
    host.cv.notify_one();
}

/// Park `me` (resumable from `token`, if any) and run `other`. Returns when `me` is resumed, or false when
/// a state load asked this host to abandon its frames: the caller then unwinds (`hle::OSLoadContext`).
/// Only `other` is woken: a shared condvar would wake every parked host thread on each switch.
pub fn switch_to(me: usize, other: usize, token: Option<(u32, ParkKind)>) -> bool {
    SWITCHES.fetch_add(1, Ordering::Relaxed);
    with(|s| {
        if let Some((ctx, kind)) = token {
            s.threads[me].park = Some((ctx, kind));
            s.parked.insert(ctx, (me, fingerprint(ctx)));
        }
        if let Some((ctx, _)) = s.threads[other].park {
            s.parked.remove(&ctx);
        }
        wake(s, other);
    });
    wait_turn(me)
}

/// Start a new guest thread running `body` and switch to it; returns when `me` is resumed.
pub fn spawn_and_switch(
    me: usize,
    token: Option<(u32, ParkKind)>,
    ctx: Box<Context>,
    body: impl FnOnce(*mut Context) + Send + 'static,
) -> bool {
    let mut ctx = ctx;
    let ctx_ptr: *mut Context = &mut *ctx;
    let id = with(|s| {
        s.threads.push(Thread {
            ctx: ctx_ptr,
            _own: Some(ctx),
            last_save: None,
            irq_ctx: None,
            irq_frames: Vec::new(),
            park: None,
            host: None,
            start: Some(Start::New(Box::new(body))),
            exited: false,
        });
        s.threads.len() - 1
    });
    switch_to(me, id, token)
}

fn spawn_host(host: Arc<Host>) {
    std::thread::Builder::new()
        .name("guest-thread".into())
        .stack_size(64 << 20)
        .spawn(move || {
            HOST.with(|h| *h.borrow_mut() = Some(host.clone()));
            serve(&host)
        })
        .expect("spawn guest thread");
}

/// The life of a host thread: run the guest thread it is given; when that one is abandoned by a state
/// load, wait to be given another.
fn serve(host: &Arc<Host>) -> ! {
    loop {
        let id = {
            let mut g = lock();
            if host.job.load(Ordering::Acquire) == NO_JOB {
                host.killed.store(false, Ordering::Release);
                g.as_mut().unwrap().idle.push(host.clone());
                IDLE_CV.notify_all();
                while host.job.load(Ordering::Acquire) == NO_JOB {
                    g = host.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                }
            }
            host.job.swap(NO_JOB, Ordering::AcqRel)
        };
        run_job(id);
        if !UNWINDING.with(|u| u.replace(false)) {
            // A guest thread that returns from its exit stub has nothing left to run: park for good.
            with(|s| s.threads[id].exited = true);
            loop {
                std::thread::park();
            }
        }
    }
}

/// Run guest thread `id` on this host until it ends or this host is asked to abandon it.
fn run_job(id: usize) {
    let (ctx, start) = with(|s| {
        let t = &mut s.threads[id];
        (t.ctx, t.start.take())
    });
    crate::abi::set_current(ctx);
    match start {
        Some(Start::New(body)) => body(ctx),
        // SAFETY: the context belongs to this thread alone while it has the baton.
        Some(Start::Resume { running }) => {
            crate::hle::resume_thread(unsafe { &mut *ctx }, id, running)
        }
        None => crate::abi::die_str("a guest thread was started twice"),
    }
}

/// The boot thread's loop after a state load abandoned its frames: it keeps serving as a host. Called
/// by the code that ran the boot function when that function returned because of the unwinding.
pub fn serve_boot() -> ! {
    UNWINDING.with(|u| u.set(false));
    let host = this_host();
    serve(&host)
}

// ---- save states -------------------------------------------------------------------------------------

/// One thread of a save state.
pub struct ThreadImage {
    /// The `Context`, byte for byte.
    pub ctx: Vec<u8>,
    pub last_save: Option<u32>,
    pub irq_ctx: Option<u32>,
    pub irq_frame: Option<IrqFrame>,
    /// Where it is parked (not set for the running thread).
    pub park: Option<(u32, ParkKind)>,
}

/// The threads of a machine at a safe point; `threads[0]` is the running one.
pub struct Image {
    pub threads: Vec<ThreadImage>,
}

fn context_bytes(c: *const Context) -> Vec<u8> {
    // SAFETY: Context is plain data.
    unsafe { std::slice::from_raw_parts(c as *const u8, std::mem::size_of::<Context>()) }.to_vec()
}

/// The threads that can still run: the running one and those parked at a token the game has not
/// rewritten. None when a thread is inside an interrupt delivery the state cannot describe (the
/// running thread in any, the others in more than one): the caller tries again at the next safe point.
pub fn capture() -> Option<Image> {
    with(|s| {
        let run = &s.threads[s.running];
        if !run.irq_frames.is_empty() {
            return None;
        }
        let mut parked: Vec<(u32, usize)> = s
            .parked
            .iter()
            .filter(|(&ctx, &(t, fp))| t != s.running && fp == fingerprint(ctx))
            .map(|(&ctx, &(t, _))| (ctx, t))
            .collect();
        parked.sort_unstable();
        let mut threads = vec![ThreadImage {
            ctx: context_bytes(run.ctx),
            last_save: run.last_save,
            irq_ctx: run.irq_ctx,
            irq_frame: None,
            park: None,
        }];
        for (_, t) in parked {
            let t = &s.threads[t];
            // a nested delivery needs the handler's frames, which a state does not describe
            let in_irq = matches!(t.park, Some((_, ParkKind::AfterIrq)));
            if t.irq_frames.len() > 1 || t.park.is_none() || in_irq != (t.irq_frames.len() == 1) {
                return None;
            }
            threads.push(ThreadImage {
                ctx: context_bytes(t.ctx),
                last_save: t.last_save,
                irq_ctx: t.irq_ctx,
                irq_frame: t.irq_frames.last().cloned(),
                park: t.park,
            });
        }
        Some(Image { threads })
    })
}

/// Make every other host abandon its frames and wait until each has gone idle. The caller (the running
/// host, at a safe point) then installs the loaded state with `install` and unwinds itself.
pub fn kill_others() {
    let me = this_host();
    let victims: Vec<Arc<Host>> = with(|s| {
        s.threads
            .iter()
            .filter(|t| !t.exited)
            .filter_map(|t| t.host.clone())
            .filter(|h| !Arc::ptr_eq(h, &me))
            .collect()
    });
    let mut g = lock();
    for v in &victims {
        v.killed.store(true, Ordering::Release);
        v.cv.notify_one();
    }
    loop {
        let s = g.as_ref().unwrap();
        if victims
            .iter()
            .all(|v| s.idle.iter().any(|i| Arc::ptr_eq(i, v)))
        {
            break;
        }
        g = IDLE_CV.wait(g).unwrap_or_else(|e| e.into_inner());
    }
}

/// Replace the threads by those of `img` (after `kill_others`, and after guest memory was restored: the
/// parked threads' fingerprints read it). The calling host takes over the running thread once it has
/// unwound to its base; the others get a host when first switched to.
pub fn install(img: &Image, ram: *mut u8) {
    let me = this_host();
    let mut threads = Vec::new();
    for (i, t) in img.threads.iter().enumerate() {
        let mut c = Context::new(ram);
        // SAFETY: Context is plain data and `ctx` holds one.
        unsafe {
            std::ptr::copy_nonoverlapping(
                t.ctx.as_ptr(),
                &mut *c as *mut Context as *mut u8,
                std::mem::size_of::<Context>(),
            );
        }
        c.ram = ram;
        let ptr: *mut Context = &mut *c;
        threads.push(Thread {
            ctx: ptr,
            _own: Some(c),
            last_save: t.last_save,
            irq_ctx: t.irq_ctx,
            irq_frames: t.irq_frame.clone().into_iter().collect(),
            park: t.park,
            host: None,
            start: Some(Start::Resume { running: i == 0 }),
            exited: false,
        });
    }
    let mut g = lock();
    let old = g.take().expect("scheduler not initialised");
    let mut parked = HashMap::new();
    for (i, t) in threads.iter().enumerate().skip(1) {
        if let Some((ctx, _)) = t.park {
            parked.insert(ctx, (i, fingerprint(ctx)));
        }
    }
    threads[0].host = Some(me.clone());
    me.job.store(0, Ordering::Release);
    *g = Some(State {
        threads,
        running: 0,
        parked,
        idle: old.idle,
    });
    let mut grave = GRAVE.lock().unwrap_or_else(|e| e.into_inner());
    grave.clear();
    grave.extend(old.threads);
    drop(g);
    set_unwinding();
}
