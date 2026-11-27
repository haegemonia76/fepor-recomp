# 0035 Save states and rewind

Status: accepted (ticket #35; amends ADR 0005 and 0012, builds on 0024)

A save state is the whole machine as one byte string: taken and loaded by the guest at a poll point, kept in slots on disk, and kept in a rewind buffer in memory. The hard part was the threads: every guest `OSThread` has a host thread with a host stack (ADR 0012), and a host stack cannot be written to a file. This ADR decides how, and records the prototype that decided it.

## Options for the fibers

1. **Copy the host stacks.** The stacks of the native backend hold absolute pointers (saved frame pointers, return addresses into JIT code, pointers to each thread's `Context` box, Rust frames of the interrupt delivery with a `Condvar`/`MutexGuard` behind them). A copy only works in the same process, never from a file or a later run, and it needs the stacks to live at fixed addresses (our own fibers instead of host threads). Rejected: it would give in-memory states only, and a second thread implementation.
2. **Save only when all threads are parked at known points.** There is no such moment: every thread that is not running is parked inside the HLE `OSLoadContext` under a chain of host frames that mirrors its guest call chain, and the running thread is in the middle of a guest function. "Known points" would have to be forced by changing the scheduler's behaviour. Rejected.
3. **Rebuild the fibers from the guest's own state.** Chosen. It is possible because of two properties of the native backend (ADR 0024) that already hold: at every call the compiled function stores all its registers to the `Context` and reloads them after the call (`out`/`inn`), so a host frame between calls carries no state that the `Context` and the guest stack do not have; and the guest returns with `blr`, which the host frames mirror, so when a function returns its `LR` is the address of the call site in its caller. The host frames of a thread are therefore a function of its `Context`, its guest stack and the place it is parked at.

## How a thread is rebuilt

- **Resume variant** (`Jit::resume_entry(addr)`, `translate::compile(.., resume = true)`): a second compilation of the function that contains `addr`, entered through a `Switch` on `Context::spr[1021]` instead of at the start. Its entries are the instruction after every call (`bl`, `bcl`, `blrl`, `bctrl`) and every branch target. It loads the registers like the code after a call does, then runs the normal translation, so there is no second semantics to keep right. Variants are compiled when first asked for and are not in the call table: normal execution is unchanged (no extra load or branch in compiled code, except the unwind check after the poll, which is on the cold path).
- **The chain** (`hle::resume_thread`, `resume_chain`): run the variant at the resume address; when it returns, `LR` names the call site in the caller, which is resumed in turn; when `LR` is a function start (the SDK's thread exit stub), call it and finish, as a thread started by `OSLoadContext` does. No stack walking is needed: the guest's epilogues restore `LR` and the callee-saved registers exactly as they do in a normal run.
- **Where a thread resumes.** A thread parked at its save point resumes after the `bl OSSaveContext` (the `srr0` of its context, as before). A thread parked inside an interrupt handler resumes where the interrupt hit: every generated loop writes the address it continues at to `spr[1020]` before it polls, delivery copies it with the interrupted FPRs into an `IrqFrame` kept with the thread (`sched::enter_irq`), and the rebuilt thread restores the integer registers from the exception context as `OSLoadContext` does, the FPRs from the frame, and continues there. The running thread resumes at its own poll address.
- **Safe point.** A state is taken only in `fepor_poll` (the safe point where interrupts are delivered), by the running thread, with every register in its `Context`, and only when the running thread is not inside an interrupt delivery and no parked thread is inside a nested one (a handler's host frames are not described by a state: `sched::capture` returns nothing and the poll tries again at the next safe point). Loading has no such restriction.
- **Host threads are workers** (`sched::Host`). A guest thread without a host gets an idle one when first switched to. A load makes every other host abandon its frames: `kill_others` sets a flag on each parked host, whose `OSLoadContext` then returns with the unwind status that compiled code already honours for threads resumed from an interrupt (every call site returns when it sees `spr[1022]`; `hle::run` and the delivery return early while `sched::unwinding()`; the poll in generated loops now checks the flag too). Each host goes idle, and the loading host unwinds itself, becomes the host of the loaded running thread and serves the others as they are switched to. One load, no thread leaked: the pool is bounded by the largest number of threads seen. The loading thread waits for every other host to be idle before it touches the machine, so no unwinding frame runs concurrently with the loaded guest.

## What a state holds

Sections (`fepor_core::snap`: tag, length, body; a reader that runs out of bytes fails cleanly):

- `MACH`: time base, function-entry counter, CPU-wide SPRs, decrementer deadline. The wall-clock-driven devices store times relative to now (`state::until`/`from_now`) and the time base is re-based, so the guest's clock continues instead of jumping.
- `RAM `: the 24 MiB of main memory.
- `HW  `: the register file, locked cache, VI interrupt state and next retrace, EXI (SRAM, RTC, channels) with the **memory card images and their command state machines**, DSP with the 16 MiB of ARAM and the AX HLE state (voices live in guest RAM), DI, AI (sample counter), SI, the pending write-gather FIFO, the pixel-engine interrupt state, the GX pipeline (CP/XF/BP registers, XF memory, TLUTs, a half-parsed command) and the video backend.
- Video backend (`Renderer::save_gpu_state`): the EFB colour and every EFB-to-texture copy, which live only on the GPU (a texture the game reads back from an EFB copy is not in RAM), read back in one submission. The depth buffer is not kept: it cannot be uploaded with `write_texture` (`Depth32Float` is not a copy destination), and a load can land in the middle of a frame; the rest of that one frame is drawn against a cleared depth buffer and the next frame starts with the game's own clear. States taken at another internal resolution are resampled on load. Interpolation history is dropped.
- Audio: samples queued for output but not played belong to another moment and are discarded on load (`AudioSink::discard`).
- `THRD`: per thread, the `Context` byte for byte, its saved-context token, interrupt state and `IrqFrame`.
- Not kept: settings, controller state (the pad is read live), the disc (the same disc must be inserted; a state names the game id, maker, disc number and version and is refused otherwise), the JIT's code, caches that are rebuilt (decoded textures, pipelines).

A state is only valid for the same layout (`VERSION` in `runtime/src/state.rs`) and, because it embeds the memory card, for a run with the same card present.

## The memory card goes back with the state

The game keeps its own copy of the card's directory and block map in RAM, so a state that restored RAM without the card would let the game write through a stale map and corrupt the card. The card image is part of the state and is written back to the card file when it differs. The consequence is the one every emulator's states have: loading a state taken before a save undoes that save. The overlay page says so. Rewind states hold the card too (it is a delta of zero pages unless a save happened).

## Slots and files

Eight slots in `<data>/states/<game id>-v<disc version>/slotN.fst`: a header (magic and the 8 bytes of disc id), then the state deflated. Writing runs on its own thread through a temporary file; reading and inflating run on their own thread and hand the state to the guest, which applies it at its next safe point. Keys: F5 save, F7 load, F6 next slot (handled where F1 and F2 are, `overlay::Shared::window_event`, and swallowed there); the Save states page of the overlay lists the slots with age and size and has Save and Load buttons; messages appear at the bottom left for four seconds (`states::message`).

## Rewind

`states.rewind` (off by default: it costs memory and a few milliseconds a second) keeps the last `states.rewind_seconds` (10 to 600, default 60) seconds in at most 512 MiB. Every second the guest takes a state at a safe point and hands it to a thread, which keeps the newest state whole and turns the previous one into a delta against it (4 KiB pages that differ, xor, deflate at the fastest level; a page-wise comparison of 45 MB is the cost, not the compression). Holding F8 pops one state back every 120 ms: the first step loads the newest state, each next one the one before, which the thread rebuilds by applying the delta to its whole copy. Letting go resumes recording from there.

## Prototype evidence (this machine, Linux, GFEP01, headless, native backend)

`FEPOR_TRACE=state` prints a hash of guest RAM and of the EFB with the retrace count when a state is captured and when one is loaded.

- Save at retrace 500, load at 800 (logo animation): RAM `cc73acc7543f008d` both times; the frame hashes of the animation (`FEPOR_TRACE=gxhash`) after the load repeat the sequence recorded after the save, frame for frame (`9815.. eb05.. c5ac.. 0e77.. 9b87.. 4c68..`).
- Save at retrace 2600 (health and safety screen), START pressed (the game moves to the Intelligent Systems logo), load at 3050: RAM `c1a5cc18f9ae7dad` both times, EFB `6f599190b133b325` both times; the frame dumps show the warning screen before the save, the logo after the change and the warning screen again after the load.
- Rewind buffer, one state a second for 45 s, then F8 held: every state popped has the RAM hash it was captured with (`a4f26fe03ddd7f5e`, `f2aea460a9723765`, `bef34bbfd61a2706`, ...), the retrace count goes back in steps of about one second, and the game plays on after each load. Taking a state of 45.5 MB costs 8 to 11 ms in the guest thread (mostly copying RAM and ARAM); a slot file of the title is 6.5 MB.
- Chapter 1 from a copy of a real card (the first dialogues, scripted input), state in slot 2 at retrace 9150 (Greil and Ike talking), the script then advances the dialogue and opens the map, load at retrace 9900: RAM `ce205a1ecac0ad3d` both times; the picture after the load is the saved dialogue, pixel for pixel in the three screenshots (`35-state-*.png` on the pull request). A second run, with the rewind buffer, held F8 for 1.2 s from retrace 10300: ten states popped (retrace 10243 down to 9757), each with the RAM hash recorded when it was captured, and the game played on to retrace 13653 without a fault. A state taken in the middle of a frame (EFB hash `9b986c42868b391f`, not black) came back with the same EFB hash.

## Limits

- Native backend only. The generated C++ backend has no resume variants: loading is refused with a message (taking one would succeed but is pointless). `FEPOR_TRACE=calls` (polling from function entries, where no resume address is known) is not supported either.
- The depth buffer and the interpolation history are not restored (see above). A state taken while a handler runs, or while a thread is parked inside a nested handler, waits for a safe moment (a few polls in practice; after 20 000 failed attempts the request is dropped with a message).
- States are not compatible across builds that change what a component writes (`VERSION`) or across the two game versions; they are refused with a message.
- A capture stalls the guest for 8 to 11 ms (the copy of 24 MiB of RAM and 16 MiB of ARAM). With the rewind buffer on that is once a second; a faster capture would need dirty-page tracking of RAM, which the compiled code's stores do not have.
- Checked: Linux x86-64 only. Windows, macOS, arm64 and a gamepad shortcut were not tried.
