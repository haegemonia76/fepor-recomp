//! What the statistics overlay shows: a snapshot the renderer publishes a few times per second.

/// One thread's share of a CPU core over the last sampling period (100 = one full core).
#[derive(Clone, Debug, Default)]
pub struct ThreadLoad {
    pub name: String,
    pub percent: f32,
}

#[derive(Clone, Debug, Default)]
pub struct StatsSnapshot {
    /// Game frames per second (what the guest renders).
    pub guest_fps: f32,
    /// Images actually presented per second (more than `guest_fps` when interpolating).
    pub shown_fps: f32,
    pub display_hz: f32,
    /// Interval between the last guest frames, oldest first, in milliseconds.
    pub frame_ms: Vec<f32>,
    /// Time per second the guest thread spends turning GX commands into GPU work / showing interpolated
    /// images, in milliseconds (1000 = the thread does nothing else).
    pub draw_ms_per_s: f32,
    pub interp_ms_per_s: f32,
    pub draws_per_frame: u32,
    /// Process CPU use in percent of one core, resident memory in MiB (Linux only).
    pub cpu_process: Option<f32>,
    pub rss_mib: Option<f32>,
    /// Busiest threads first (Linux only).
    pub threads: Vec<ThreadLoad>,
    /// GPU utilisation in percent when the driver tells (AMD on Linux).
    pub gpu_busy: Option<f32>,
    /// How long the last batch of GPU work took from submission to completion.
    pub gpu_latency_ms: f32,
    /// GPU time of the scaling pass (FSR and filters), when the GPU can time it.
    pub scaling_gpu_ms: Option<f32>,
    pub gpu_name: String,
    pub backend: String,
    /// Render target size in pixels.
    pub target: (u32, u32),
    /// Audio frames the mixer produced per second and audio buffers handed to the output per second
    /// (both 200 when the game keeps up).
    pub ax_per_s: f32,
    pub ai_per_s: f32,
}

impl StatsSnapshot {
    /// Average, 1 % worst and worst frame interval in `frame_ms`.
    pub fn frame_summary(&self) -> Option<(f32, f32, f32)> {
        if self.frame_ms.is_empty() {
            return None;
        }
        let avg = self.frame_ms.iter().sum::<f32>() / self.frame_ms.len() as f32;
        let mut sorted = self.frame_ms.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let p99 = sorted[((sorted.len() as f32 * 0.99) as usize).min(sorted.len() - 1)];
        Some((avg, p99, *sorted.last().unwrap_or(&0.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reports_average_percentile_and_worst() {
        let s = StatsSnapshot {
            frame_ms: (1..=100).map(|i| i as f32).collect(),
            ..Default::default()
        };
        let (avg, p99, worst) = s.frame_summary().unwrap();
        assert_eq!((avg, p99, worst), (50.5, 100.0, 100.0));
        assert!(StatsSnapshot::default().frame_summary().is_none());
    }
}
