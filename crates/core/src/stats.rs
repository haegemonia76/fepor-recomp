//! Counters the runtime bumps and the statistics overlay reads (cumulative since start; readers difference them).

use std::sync::atomic::AtomicU64;

/// Audio frames (5 ms each) produced by the AX mixer.
pub static AX_FRAMES: AtomicU64 = AtomicU64::new(0);
/// Audio buffers (5 ms each) handed to the audio output.
pub static AI_BUFFERS: AtomicU64 = AtomicU64::new(0);
/// Game frames (efb copies to the external framebuffer) the renderer has finished.
pub static GAME_FRAMES: AtomicU64 = AtomicU64::new(0);
