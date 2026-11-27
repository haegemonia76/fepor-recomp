//! What the runtime tells a bug report (ADR 0047): retraces, thread counts per state, patch sets.

/// A text block with the guest machine's state. Reads guest memory the way the watchdog does, while the
/// guest thread keeps running: enough for a report, not for exact numbers.
pub fn state() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let m = crate::machine();
    let mut t = format!(
        "video retraces: {}\nhost thread switches: {}\naudio: {} AX frames, {} AI buffers\n",
        fepor_core::stats::RETRACES.load(Relaxed),
        crate::sched::switches(),
        fepor_core::stats::AX_FRAMES.load(Relaxed),
        fepor_core::stats::AI_BUFFERS.load(Relaxed),
    );
    // OS threads: __OSActiveThreadQueue head at 0x800000DC, linkActive at +0x2FC, state at +0x2C8 (high half).
    let mut counts = [0u32; 16];
    let (mut n, mut th) = (0u32, m.read_u32(0x8000_00DC));
    while th != 0 && n < 64 {
        let state = (m.read_u32(th + 0x2C8) >> 16) as usize;
        counts[state.min(15)] += 1;
        n += 1;
        th = m.read_u32(th + 0x2FC);
    }
    let name = |s: usize| match s {
        1 => "ready",
        2 => "running",
        4 => "waiting",
        8 => "moribund",
        _ => "other",
    };
    t.push_str(&format!("guest threads: {n}"));
    for (s, c) in counts.iter().enumerate().filter(|(_, c)| **c > 0) {
        t.push_str(&format!(", {c} {} (state {s})", name(s)));
    }
    let patches = crate::patch::summary();
    t.push_str(&format!(
        "\npatch engine:\n{}",
        if patches.is_empty() { "no patch sets registered\n" } else { &patches }
    ));
    t
}
