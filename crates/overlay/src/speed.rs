//! The fast-forward and slow-motion keys (ADR 0036): held, so releasing them returns to normal speed.

use std::sync::atomic::{AtomicBool, Ordering};

use fepor_core::settings::SettingsHandle;
use fepor_core::speed::{self, Speed};

#[derive(Default)]
pub struct SpeedKeys {
    fast: AtomicBool,
    slow: AtomicBool,
}

impl SpeedKeys {
    /// The key for fast-forward (`fast`) or slow motion went down or up. Fast-forward wins when both are held.
    pub fn hold(&self, settings: &SettingsHandle, fast: bool, down: bool) {
        if fast {
            self.fast.store(down, Ordering::Release);
        } else {
            self.slow.store(down, Ordering::Release);
        }
        self.apply(settings);
    }

    /// Let go of both (the window lost the focus, the overlay opened).
    pub fn release(&self, settings: &SettingsHandle) {
        self.fast.store(false, Ordering::Release);
        self.slow.store(false, Ordering::Release);
        self.apply(settings);
    }

    fn apply(&self, settings: &SettingsHandle) {
        let g = settings.get().game;
        speed::set(if self.fast.load(Ordering::Acquire) {
            g.fast_forward
        } else if self.slow.load(Ordering::Acquire) {
            g.slow_motion
        } else {
            Speed::Normal
        });
    }
}
