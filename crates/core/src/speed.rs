//! Game speed (ADR 0025): fast-forward and slow motion.
//!
//! The runtime's clock (time base, retrace, audio DMA) runs at `factor()` times real time, so the whole machine
//! speeds up or slows down together. The factor is set by the window's hotkeys (hold) or `FEPOR_SPEED` and read by
//! the runtime, the renderer and the audio output.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

/// The most "uncapped" ever asks of the clock, and where it starts (see [`adapt`]).
pub const UNCAPPED_MAX: f32 = 32.0;
const UNCAPPED_START_MILLI: u32 = 4000;

/// A speed the player can pick for fast-forward or slow motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speed {
    Quarter,
    Half,
    #[default]
    Normal,
    X2,
    X4,
    Uncapped,
}

impl Speed {
    /// Offered for the fast-forward hotkey.
    pub const FAST: [Speed; 3] = [Speed::X2, Speed::X4, Speed::Uncapped];
    /// Offered for the slow-motion hotkey.
    pub const SLOW: [Speed; 2] = [Speed::Half, Speed::Quarter];

    pub fn label(self) -> &'static str {
        match self {
            Speed::Quarter => "x0.25",
            Speed::Half => "x0.5",
            Speed::Normal => "x1",
            Speed::X2 => "x2",
            Speed::X4 => "x4",
            Speed::Uncapped => "Uncapped",
        }
    }

    /// Guest seconds per real second.
    pub fn factor(self) -> f32 {
        match self {
            Speed::Quarter => 0.25,
            Speed::Half => 0.5,
            Speed::Normal => 1.0,
            Speed::X2 => 2.0,
            Speed::X4 => 4.0,
            Speed::Uncapped => UNCAPPED_MAX,
        }
    }

    /// Parse the `FEPOR_SPEED` syntax: `0.25`, `0.5`, `1`, `2`, `4`, `max` (a leading `x` is allowed).
    pub fn parse(s: &str) -> Option<Speed> {
        let s = s.trim().trim_start_matches(['x', 'X']);
        match s {
            "max" | "uncapped" => Some(Speed::Uncapped),
            _ => {
                let f: f32 = s.parse().ok()?;
                [
                    Speed::Quarter,
                    Speed::Half,
                    Speed::Normal,
                    Speed::X2,
                    Speed::X4,
                ]
                .into_iter()
                .find(|v| v.factor() == f)
            }
        }
    }
}

/// What happens to the game's sound while it does not run at normal speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeedAudio {
    /// Played faster or slower, pitch following (like tape). Uncapped speed is always silent.
    #[default]
    Pitch,
    Mute,
}

impl SpeedAudio {
    pub const ALL: [SpeedAudio; 2] = [SpeedAudio::Pitch, SpeedAudio::Mute];

    pub fn label(self) -> &'static str {
        match self {
            SpeedAudio::Pitch => "Pitch follows the speed",
            SpeedAudio::Mute => "Silent",
        }
    }
}

/// The factor the clock runs at, in thousandths. Fixed except for uncapped, which [`adapt`] steers.
static MILLI: AtomicU32 = AtomicU32::new(1000);
static UNCAPPED: AtomicBool = AtomicBool::new(false);

pub fn set(speed: Speed) {
    let uncapped = speed == Speed::Uncapped;
    UNCAPPED.store(uncapped, Ordering::Release);
    let milli = if uncapped {
        UNCAPPED_START_MILLI
    } else {
        (speed.factor() * 1000.0) as u32
    };
    MILLI.store(milli, Ordering::Release);
}

/// Guest seconds per real second right now.
pub fn factor() -> f32 {
    MILLI.load(Ordering::Acquire) as f32 / 1000.0
}

/// The same in thousandths (the runtime clock's integer arithmetic).
pub fn milli() -> u32 {
    MILLI.load(Ordering::Acquire)
}

/// Faster than real time: the renderer must not wait for the display.
pub fn is_fast() -> bool {
    milli() > 1000
}

/// "Uncapped" is selected: the factor follows what the host can do.
pub fn is_uncapped() -> bool {
    UNCAPPED.load(Ordering::Acquire)
}

/// Steer the uncapped factor: the machine reports (a few times a second) whether the guest fell behind
/// its retraces. The guest's CPU and the DSP emulation cost grow with guest time, so there is a factor
/// above which nothing gets done at all (x16 hung the boot on the test machine); lagging lowers the factor
/// by a quarter, keeping up raises it by 15 %, up to [`UNCAPPED_MAX`]. Does nothing at a fixed speed.
pub fn adapt(lagging: bool) {
    if !is_uncapped() {
        return;
    }
    let m = milli();
    let next = if lagging {
        (m * 3 / 4).max(1000)
    } else {
        (m * 23 / 20).min((UNCAPPED_MAX * 1000.0) as u32)
    };
    MILLI.store(next, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_environment_syntax() {
        assert_eq!(Speed::parse("2"), Some(Speed::X2));
        assert_eq!(Speed::parse("x4"), Some(Speed::X4));
        assert_eq!(Speed::parse("0.5"), Some(Speed::Half));
        assert_eq!(Speed::parse("max"), Some(Speed::Uncapped));
        assert_eq!(Speed::parse("3"), None);
    }

    #[test]
    fn the_shared_factor_follows_set() {
        set(Speed::Quarter);
        assert_eq!(milli(), 250);
        assert!(!is_fast());
        set(Speed::Uncapped);
        assert!(is_fast() && is_uncapped());
        assert_eq!(milli(), 4000);
        adapt(false);
        assert_eq!(milli(), 4600);
        adapt(true);
        assert_eq!(milli(), 3450);
        set(Speed::X2);
        adapt(true);
        assert_eq!(milli(), 2000);
        set(Speed::Normal);
        assert_eq!(factor(), 1.0);
    }
}
