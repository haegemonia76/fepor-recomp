//! Process, thread and GPU load for the statistics overlay. Linux reads /proc and sysfs; elsewhere the
//! values are simply absent.

use std::time::Instant;

use fepor_overlay::ThreadLoad;

/// Clock ticks per second of the `/proc/*/stat` CPU times (always 100 on Linux).
const TICKS: f32 = 100.0;

/// CPU time (user + system, in ticks) from the contents of a `/proc/.../stat` file, and the command name.
fn parse_stat(text: &str) -> Option<(String, u64)> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text[open + 1..close].to_string();
    // the fields after the name start at field 3 (state); utime and stime are fields 14 and 15
    let mut rest = text[close + 1..].split_whitespace();
    let utime: u64 = rest.nth(11)?.parse().ok()?;
    let stime: u64 = rest.next()?.parse().ok()?;
    Some((name, utime + stime))
}

fn read_stat(path: &str) -> Option<(String, u64)> {
    parse_stat(&std::fs::read_to_string(path).ok()?)
}

pub struct Sample {
    pub cpu_process: Option<f32>,
    pub rss_mib: Option<f32>,
    pub threads: Vec<ThreadLoad>,
}

/// Differences CPU times between two calls.
pub struct CpuSampler {
    at: Instant,
    process: Option<u64>,
    threads: std::collections::HashMap<String, (String, u64)>,
}

impl CpuSampler {
    pub fn new() -> CpuSampler {
        CpuSampler {
            at: Instant::now(),
            process: None,
            threads: Default::default(),
        }
    }

    pub fn sample(&mut self) -> Sample {
        let dt = self.at.elapsed().as_secs_f32().max(0.001);
        self.at = Instant::now();
        let process = read_stat("/proc/self/stat").map(|(_, t)| t);
        let cpu_process = match (process, self.process) {
            (Some(now), Some(before)) => Some((now - before.min(now)) as f32 / TICKS / dt * 100.0),
            _ => None,
        };
        self.process = process;
        let mut threads = Vec::new();
        let mut now_threads = std::collections::HashMap::new();
        if let Ok(dir) = std::fs::read_dir("/proc/self/task") {
            for e in dir.flatten() {
                let tid = e.file_name().to_string_lossy().into_owned();
                if let Some((name, ticks)) = read_stat(&format!("/proc/self/task/{tid}/stat")) {
                    if let Some((_, before)) = self.threads.get(&tid) {
                        let percent = (ticks - before.min(&ticks)) as f32 / TICKS / dt * 100.0;
                        threads.push(ThreadLoad {
                            name: name.clone(),
                            percent,
                        });
                    }
                    now_threads.insert(tid, (name, ticks));
                }
            }
        }
        self.threads = now_threads;
        threads.sort_by(|a, b| b.percent.total_cmp(&a.percent));
        let rss_mib = std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|t| t.split_whitespace().nth(1)?.parse::<f32>().ok())
            .map(|pages| pages * 4096.0 / 1_048_576.0);
        Sample {
            cpu_process,
            rss_mib,
            threads,
        }
    }
}

/// The sysfs file with the GPU's utilisation (AMD), for the adapter with this PCI vendor and device id.
pub fn gpu_busy_path(vendor: u32, device: u32) -> Option<std::path::PathBuf> {
    let hex = |p: std::path::PathBuf| {
        u32::from_str_radix(
            std::fs::read_to_string(p)
                .ok()?
                .trim()
                .trim_start_matches("0x"),
            16,
        )
        .ok()
    };
    for e in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let dir = e.path().join("device");
        if hex(dir.join("vendor")) == Some(vendor) && hex(dir.join("device")) == Some(device) {
            let busy = dir.join("gpu_busy_percent");
            if busy.exists() {
                return Some(busy);
            }
        }
    }
    None
}

pub fn read_gpu_busy(path: &std::path::Path) -> Option<f32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_spaces_and_parentheses_in_the_name() {
        let line = "1234 (we (ird) name) S 1 2 3 4 5 6 7 8 9 10 111 222 0 0 20 0 1 0 5 100 200";
        assert_eq!(parse_stat(line), Some(("we (ird) name".to_string(), 333)));
    }
}
