//! Playtime per game, kept in `<data>/playtime.toml` (ADR 0025).
//!
//! The data folder is the player's profile (ADR 0010): its saves, settings and this file belong together.
//! The file holds whole seconds per game id (`[seconds]`, one entry per disc region played). A file that
//! cannot be read is set aside as `playtime.toml.corrupt` and counting starts again at zero: losing a
//! statistic is better than refusing to start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playtime {
    /// Seconds played, by game id (`GFEP01`).
    seconds: BTreeMap<String, u64>,
}

impl Playtime {
    /// Load `path`. A missing file is an empty record; an unreadable one is renamed to `<name>.corrupt`
    /// (with a message) and replaced by an empty record.
    pub fn load(path: &Path) -> Playtime {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Playtime::default(),
            Err(e) => {
                eprintln!("playtime: {}: {e}; starting at zero", path.display());
                return Playtime::default();
            }
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".corrupt");
            eprintln!(
                "playtime: {}: {e}; kept as {}, starting at zero",
                path.display(),
                PathBuf::from(&aside).display()
            );
            let _ = std::fs::rename(path, aside);
            Playtime::default()
        })
    }

    /// Write to `path` through a temporary file, so that a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        std::fs::write(
            &tmp,
            toml::to_string_pretty(self).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(tmp, path)
    }

    pub fn get(&self, game_id: &str) -> u64 {
        self.seconds.get(game_id).copied().unwrap_or(0)
    }

    pub fn add(&mut self, game_id: &str, secs: u64) {
        let s = self.seconds.entry(game_id.to_string()).or_insert(0);
        *s = s.saturating_add(secs);
    }

    /// Seconds played over every game of the folder.
    pub fn total(&self) -> u64 {
        self.seconds.values().fold(0, |a, s| a.saturating_add(*s))
    }
}

/// The playtime file of a data folder.
pub fn path_in(data: &Path) -> PathBuf {
    data.join("playtime.toml")
}

/// "3 h 12 min", "12 min", "under a minute".
pub fn format(secs: u64) -> String {
    match (secs / 3600, secs / 60 % 60) {
        (0, 0) => "under a minute".to_string(),
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} h {m:02} min"),
    }
}

/// Counts the time between calls to [`Counter::tick`] during which the game was running. Whole seconds
/// are handed out; the remainder is carried to the next call.
pub struct Counter {
    last: Instant,
    carry: Duration,
}

impl Counter {
    pub fn new() -> Counter {
        Counter {
            last: Instant::now(),
            carry: Duration::ZERO,
        }
    }

    /// Whole seconds to add now. `running` says whether the time since the previous call counted.
    pub fn tick(&mut self, running: bool) -> u64 {
        self.tick_at(Instant::now(), running)
    }

    fn tick_at(&mut self, now: Instant, running: bool) -> u64 {
        if running {
            self.carry += now.saturating_duration_since(self.last);
        }
        self.last = now;
        let whole = self.carry.as_secs();
        self.carry -= Duration::from_secs(whole);
        whole
    }
}

impl Default for Counter {
    fn default() -> Self {
        Counter::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_accumulate_across_sessions_and_games() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(dir.path());
        let mut p = Playtime::load(&path);
        assert_eq!(p.total(), 0);
        p.add("GFEP01", 90);
        p.save(&path).unwrap();
        let mut p = Playtime::load(&path);
        p.add("GFEP01", 30);
        p.add("GFEJ01", 5);
        p.save(&path).unwrap();
        let p = Playtime::load(&path);
        assert_eq!((p.get("GFEP01"), p.get("GFEJ01"), p.total()), (120, 5, 125));
        assert!(!dir.path().join("playtime.toml.tmp").exists());
    }

    #[test]
    fn a_corrupted_file_is_set_aside_and_counting_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(dir.path());
        std::fs::write(&path, "seconds = [not toml").unwrap();
        assert_eq!(Playtime::load(&path), Playtime::default());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("playtime.toml.corrupt")).unwrap(),
            "seconds = [not toml"
        );
        // wrong types are corrupt too
        std::fs::write(&path, "[seconds]\nGFEP01 = \"a lot\"\n").unwrap();
        assert_eq!(Playtime::load(&path).total(), 0);
    }

    #[test]
    fn counter_counts_only_running_time_and_carries_the_remainder() {
        let t0 = Instant::now();
        let mut c = Counter {
            last: t0,
            carry: Duration::ZERO,
        };
        let at = |ms| t0 + Duration::from_millis(ms);
        assert_eq!(c.tick_at(at(1500), true), 1);
        // paused (overlay open) for 10 s: nothing
        assert_eq!(c.tick_at(at(11_500), false), 0);
        // the 0.5 s carried plus 0.6 s more
        assert_eq!(c.tick_at(at(12_100), true), 1);
        assert_eq!(c.tick_at(at(12_600), true), 0);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(format(0), "under a minute");
        assert_eq!(format(59 * 60 + 59), "59 min");
        assert_eq!(format(3 * 3600 + 12 * 60), "3 h 12 min");
        assert_eq!(format(3600), "1 h 00 min");
    }
}
