//! The guest's clock: real time scaled by the game speed (`fepor_core::speed`, ADR 0025).
//!
//! The time base, the retrace, the audio DMA and the audio sample counter all measure time here, so changing
//! the speed makes the whole machine faster or slower at once and the game cannot tell. Time is in guest
//! nanoseconds since the first call; a speed change starts a new segment so that time never jumps.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone, Copy)]
struct Segment {
    /// Real nanoseconds since the epoch when the segment began, and the guest time then.
    real_ns: u64,
    guest_ns: u64,
    milli: u32,
}

/// Guest threads run on their own host threads (one at a time): the clock is process-wide.
static EPOCH: OnceLock<Instant> = OnceLock::new();
static SEGMENT: Mutex<Segment> = Mutex::new(Segment {
    real_ns: 0,
    guest_ns: 0,
    milli: 1000,
});

/// Guest nanoseconds.
pub fn now_ns() -> u64 {
    let real = EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64;
    let milli = fepor_core::speed::milli();
    let mut seg = SEGMENT.lock().unwrap_or_else(|e| e.into_inner());
    if seg.milli != milli {
        *seg = Segment {
            real_ns: real,
            guest_ns: seg.guest_ns + (real - seg.real_ns) * seg.milli as u64 / 1000,
            milli,
        };
    }
    seg.guest_ns + (real - seg.real_ns) * milli as u64 / 1000
}
