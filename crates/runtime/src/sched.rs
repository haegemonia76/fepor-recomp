//! Guest threads. Every guest OSThread runs on its own host thread, but only one runs at a time:
//! a context switch hands the baton over and parks the caller (ADR 0005, amended).
//!
//! A parked thread waits inside the runtime's OSLoadContext. It is identified by the OSContext
//! address it will be resumed from (its token): the context it last saved (resume at the save
//! point) or the context the interrupt handler saved for it (resume where it was interrupted).

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};

use crate::context::Context;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParkKind {
    /// Resume after the thread's OSSaveContext call (the generated code jumps there).
    AtSave,
    /// Resume at the instruction an interrupt interrupted (the host stack is unwound to the delivery point).
    AfterIrq,
}

struct Thread {
    ctx: *mut Context,
    last_save: Option<u32>,
    irq_ctx: Option<u32>,
    park: Option<(u32, ParkKind)>,
}

struct State {
    threads: Vec<Thread>,
    running: usize,
    parked: HashMap<u32, usize>,
}

// SAFETY: raw context pointers are only dereferenced by the thread holding the baton.
unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static CV: Condvar = Condvar::new();

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.as_mut().expect("scheduler not initialised"))
}

/// Register the boot thread (thread 0).
pub fn init(main_ctx: *mut Context) {
    let mut g = STATE.lock().unwrap();
    *g = Some(State {
        threads: vec![Thread {
            ctx: main_ctx,
            last_save: None,
            irq_ctx: None,
            park: None,
        }],
        running: 0,
        parked: HashMap::new(),
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

/// Which parked thread resumes from this context?
pub fn parked_at(ctx: u32) -> Option<usize> {
    with(|s| s.parked.get(&ctx).copied())
}

fn wait_for_turn(me: usize) {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    while g.as_ref().unwrap().running != me {
        g = CV.wait(g).unwrap_or_else(|e| e.into_inner());
    }
    crate::abi::set_current(g.as_ref().unwrap().threads[me].ctx);
}

/// Park `me` (resumable from `token`, if any) and run `other`. Returns when `me` is resumed.
pub fn switch_to(me: usize, other: usize, token: Option<(u32, ParkKind)>) {
    with(|s| {
        if let Some((ctx, kind)) = token {
            s.threads[me].park = Some((ctx, kind));
            s.parked.insert(ctx, me);
        }
        if let Some((ctx, _)) = s.threads[other].park {
            s.parked.remove(&ctx);
        }
        s.running = other;
    });
    CV.notify_all();
    wait_for_turn(me);
}

/// Start a new guest thread running `body` and switch to it; returns when `me` is resumed.
pub fn spawn_and_switch(
    me: usize,
    token: Option<(u32, ParkKind)>,
    ctx: Box<Context>,
    body: impl FnOnce(*mut Context) + Send + 'static,
) {
    let ctx_ptr = Box::into_raw(ctx);
    let ptr_for_thread = ctx_ptr as usize;
    let id = with(|s| {
        s.threads.push(Thread {
            ctx: ctx_ptr,
            last_save: None,
            irq_ctx: None,
            park: None,
        });
        s.threads.len() - 1
    });
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            wait_for_turn(id);
            body(ptr_for_thread as *mut Context);
            // A guest thread that returns from its exit stub has nothing left to run: park for good.
            loop {
                std::thread::park();
            }
        })
        .expect("spawn guest thread");
    switch_to(me, id, token);
}
