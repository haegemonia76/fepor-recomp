//! The bug report bundle (ticket #47, ADR 0047): one zip with everything a developer needs to read a
//! problem report, and nothing the player would not want to send. Shared by `fepor-recomp bug-report`,
//! the overlay's Help page and the launcher GUI.
//!
//! Inside: `report.txt` (versions, system, GPU, display, audio, controllers, settings in effect, game and
//! engine state, memory card summaries, card backup names, what was anonymised), the settings and bindings,
//! the enabled mods' manifests and the cheat selection, the logs (tail, size-capped), the crash message
//! block, and a screenshot. Never: card images, saves, game data, generated code, mod or cheat payloads.
//! Every text goes through [`crate::anonymise::Anonymiser`].
//!
//! What only the running game knows is kept in `<data>/logs/session.toml` ([`init`], [`note`]) so the CLI
//! can report it after the game ended; state that changes (frame times, threads, the current picture) is
//! handed to [`create`] by the process that runs the game, as [`Extra`].

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::anonymise::{Anonymiser, Identity};

/// Longest tail of one log or text file that goes into a bundle.
const TEXT_CAP: usize = 512 << 10;
/// Longest tail of a small settings file.
const SMALL_CAP: usize = 64 << 10;
/// Logs of earlier runs kept in a bundle besides the newest.
const MAX_LOGS: usize = 6;

/// What only the running game knows, kept in `<data>/logs/session.toml` for the next bug report.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Session {
    /// Version of the game executable and how it was built.
    pub build: String,
    /// `jit` (in-process compiler) or `cpp` (generated library).
    pub engine: String,
    pub game_id: String,
    /// `FEPOR_TRACE` of that run, when set (its output is in the log).
    pub trace: String,
    /// The folder holding the disc image or extracted game (removed from the report by the anonymiser).
    pub disc_dir: String,
    /// Named text blocks published by the crates ([`note`]): `gpu`, `audio`, `controllers`, ...
    pub sections: BTreeMap<String, String>,
}

/// What the process running the game adds when it asks for a report.
#[derive(Debug, Default)]
pub struct Extra {
    /// PNG of the picture on screen now; without one the newest in `<data>/screenshots` is used.
    pub screenshot: Option<Vec<u8>>,
    /// State that changes while the game runs: `(title, text)`.
    pub sections: Vec<(String, String)>,
}

static DATA: OnceLock<PathBuf> = OnceLock::new();
static SESSION_LOCK: Mutex<()> = Mutex::new(());

pub fn session_path(data: &Path) -> PathBuf {
    data.join("logs").join("session.toml")
}

/// Start the session record of this run in `data` (what earlier runs noted is dropped).
pub fn init(data: &Path) {
    let _ = DATA.set(data.to_path_buf());
    update_session(data, |s| *s = Session::default());
}

/// Change the session record (read, edit, write). Failures are ignored: it only feeds bug reports.
pub fn update_session(data: &Path, edit: impl FnOnce(&mut Session)) {
    let _lock = SESSION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = session_path(data);
    let mut s = read_session(data);
    edit(&mut s);
    if let (Some(dir), Ok(text)) = (path.parent(), toml::to_string(&s)) {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&path, text);
    }
}

fn read_session(data: &Path) -> Session {
    std::fs::read_to_string(session_path(data))
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

/// Record a named block of text for the next bug report (a no-op before [`init`]). Noting a name again
/// replaces it.
pub fn note(name: &str, text: impl Into<String>) {
    if let Some(data) = DATA.get() {
        let text = text.into();
        update_session(data, |s| {
            s.sections.insert(name.to_string(), text);
        });
    }
}

/// The version string of this build.
pub fn build_string() -> String {
    format!(
        "{} ({}, {}-{})",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::ARCH,
        std::env::consts::OS
    )
}

/// Folders and file types that hold saves or game data: never in a bundle, whatever asks for them.
const PRIVATE_DIRS: &[&str] = &[
    "memcards",
    "imports",
    "disc",
    "generated",
    "saves",
    "exports",
    "cheats",
];
const PRIVATE_EXTENSIONS: &[&str] = &[
    "raw", "gci", "sav", "gcm", "iso", "rvz", "gcz", "dol", "bin", "thp", "imported", "failed",
];

/// Is `rel` (a path relative to the data folder, either separator) a save or a piece of game data? Of the
/// `mods` folder only the manifests may go in, not what the mods replace.
pub fn is_excluded(rel: &str) -> bool {
    let rel = rel.to_ascii_lowercase().replace('\\', "/");
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let Some((last, dirs)) = parts.split_last() else {
        return false;
    };
    if dirs.iter().any(|d| PRIVATE_DIRS.contains(d)) {
        return true;
    }
    if dirs.first() == Some(&"mods") && !matches!(*last, "mods.toml" | "mod.toml") {
        return true;
    }
    last.rsplit_once('.')
        .is_some_and(|(_, ext)| PRIVATE_EXTENSIONS.contains(&ext))
}

#[cfg(target_os = "linux")]
fn os_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into())
}

#[cfg(not(target_os = "linux"))]
fn os_name() -> String {
    std::env::consts::OS.into()
}

/// The value after `key:` on the first line of a `/proc` style file that has it.
#[cfg(target_os = "linux")]
fn proc_value(path: &str, key: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()?.lines().find_map(|l| {
        l.strip_prefix(key)?
            .trim_start()
            .strip_prefix(':')
            .map(|v| v.trim().to_string())
    })
}

/// Operating system, kernel, CPU and memory of this machine.
pub fn system_info() -> String {
    let mut t = String::new();
    let mut line = |k: &str, v: String| t.push_str(&format!("{k}: {v}\n"));
    line(
        "OS",
        format!(
            "{} ({}, {})",
            os_name(),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    );
    #[cfg(target_os = "linux")]
    {
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        line("kernel", kernel.trim().to_string());
        line(
            "CPU",
            proc_value("/proc/cpuinfo", "model name").unwrap_or_else(|| "unknown".into()),
        );
        let mem = |k: &str| proc_value("/proc/meminfo", k).unwrap_or_else(|| "unknown".into());
        line("RAM total", mem("MemTotal"));
        line("RAM available", mem("MemAvailable"));
    }
    line(
        "logical cores",
        std::thread::available_parallelism().map_or("unknown".into(), |n| n.to_string()),
    );
    t
}

/// The last `cap` bytes of `path` as text, starting at a line; a note says how much was cut.
fn read_tail(path: &Path, cap: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() <= cap {
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    let from = bytes.len() - cap;
    let start = bytes[from..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(from, |i| from + i + 1);
    Some(format!(
        "[first {start} bytes of {} cut]\n{}",
        bytes.len(),
        String::from_utf8_lossy(&bytes[start..])
    ))
}

/// The fatal message block of a log: from the first `fepor: fatal:` / panic line to the end (the game
/// exits right after printing it), at most 200 lines.
pub fn extract_crash(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let at = lines.iter().position(|l| {
        l.starts_with("fepor: fatal:")
            || l.starts_with("fepor: watchdog:")
            || l.contains("panicked at")
    })?;
    Some(lines[at..lines.len().min(at + 200)].join("\n") + "\n")
}

/// Slot, format and saves of the raw card images, as `fepor-recomp card-info` prints them; no save contents.
fn memcard_summary(data: &Path) -> String {
    let Ok(dir) = std::fs::read_dir(data.join("memcards")) else {
        return "no memcards folder".into();
    };
    let mut files: Vec<PathBuf> = dir
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "raw"))
        .collect();
    files.sort();
    if files.is_empty() {
        return "no card images".into();
    }
    let mut t = String::new();
    for p in files {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match std::fs::read(&p)
            .map_err(|e| e.to_string())
            .and_then(|d| crate::memcard::inspect(&d))
        {
            Ok(i) => {
                t.push_str(&format!(
                    "{name}: {} Mbit, {}, header {}, directory {:?}, BAT {:?}, {} free blocks\n",
                    i.size_mbit,
                    if i.formatted {
                        "formatted"
                    } else {
                        "not formatted"
                    },
                    if i.header_ok { "ok" } else { "BAD" },
                    i.dir_ok,
                    i.bat_ok,
                    i.free_blocks
                ));
                for s in &i.saves {
                    t.push_str(&format!(
                        "  {}{} {}: {} blocks\n",
                        s.game_code, s.maker_code, s.file_name, s.blocks
                    ));
                }
            }
            Err(e) => t.push_str(&format!("{name}: cannot read as a card image ({e})\n")),
        }
    }
    t
}

/// File names and sizes in `<data>/memcards/backups`: the card images themselves are never read.
fn backups(data: &Path) -> String {
    let Ok(dir) = std::fs::read_dir(crate::memcard::backups_dir(data)) else {
        return "none".into();
    };
    let mut v: Vec<String> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            Some(format!(
                "{} ({} bytes)",
                e.file_name().to_string_lossy(),
                e.metadata().ok()?.len()
            ))
        })
        .collect();
    v.sort();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

/// `(name in the zip, file, cap)` of the text files of a data folder worth sending.
fn text_files(data: &Path) -> Vec<(String, PathBuf, usize)> {
    let mut v: Vec<(String, PathBuf, usize)> = Vec::new();
    for name in [
        "settings.toml",
        "input.toml",
        "cheats.toml",
        "playtime.toml",
        "mods/mods.toml",
    ] {
        v.push((name.into(), data.join(name), SMALL_CAP));
    }
    // each enabled mod's manifest, not its files
    for e in crate::mods::ModList::load(data)
        .entries
        .iter()
        .filter(|e| e.enabled)
    {
        let rel = format!("mods/{}/mod.toml", e.m.id);
        v.push((rel.clone(), data.join(&rel), SMALL_CAP));
    }
    // the logs, newest first: the game's own, the previous run's, then the launcher's rotated ones
    let mut logs: Vec<PathBuf> = std::fs::read_dir(data.join("logs"))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "log" || x == "txt"))
        .collect();
    logs.sort();
    logs.truncate(MAX_LOGS + 1);
    for p in logs {
        let name = format!(
            "logs/{}",
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        v.push((name, p, TEXT_CAP));
    }
    v.push(("logs/session.toml".into(), session_path(data), SMALL_CAP));
    v
}

/// The most recently written `.png` in `<data>/screenshots`.
fn newest_screenshot(data: &Path) -> Option<Vec<u8>> {
    std::fs::read_dir(data.join("screenshots"))
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("png"))
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .and_then(|(_, p)| std::fs::read(p).ok())
}

/// Write the bundle for the data folder `data` into `out` (a `.zip` path) and return it. Text is read
/// lossily (tail only when long) and passes through `anon`; a file that `is_excluded` never goes in.
pub fn create_with(
    data: &Path,
    out: &Path,
    extra: Extra,
    anon: &Anonymiser,
) -> Result<PathBuf, String> {
    let session = read_session(data);
    let mut texts: Vec<(String, String)> = Vec::new();
    for (name, path, cap) in text_files(data) {
        if is_excluded(&name) {
            continue;
        }
        if let Some(t) = read_tail(&path, cap) {
            texts.push((name, anon.apply(&t)));
        }
    }
    // the fatal message block of the newest logs
    let mut crash = String::new();
    for name in [crate::applog::LOG_NAME, crate::applog::PREV_NAME] {
        if let Some(c) = std::fs::read_to_string(data.join("logs").join(name))
            .ok()
            .as_deref()
            .and_then(extract_crash)
        {
            crash.push_str(&format!("== {name} ==\n{c}\n"));
        }
    }
    if !crash.is_empty() {
        texts.push(("crash.txt".into(), anon.apply(&crash)));
    }
    let screenshot = extra.screenshot.or_else(|| newest_screenshot(data));

    let mut names: Vec<String> = texts.iter().map(|t| t.0.clone()).collect();
    if screenshot.is_some() {
        names.push("screenshot.png".into());
    }
    let mut body = String::new();
    let mut section = |title: &str, text: &str| {
        body.push_str(&format!("\n== {title} ==\n{}\n", text.trim_end()));
    };
    let or = |v: &str| {
        if v.is_empty() {
            "unknown (the game has not run here)".to_string()
        } else {
            v.to_string()
        }
    };
    section(
        "version",
        &format!(
            "report tool: {}\ngame build: {}\nengine: {}\ngame ID: {}\ntrace (FEPOR_TRACE): {}",
            build_string(),
            or(&session.build),
            or(&session.engine),
            or(&session.game_id),
            if session.trace.is_empty() {
                "off"
            } else {
                &session.trace
            },
        ),
    );
    section("system", &system_info());
    for (name, text) in &session.sections {
        section(name, text);
    }
    if extra.sections.is_empty() {
        section("live state", "not available: the game does not run in the process that made this report (the sections above are from its last start)");
    }
    for (name, text) in &extra.sections {
        section(name, text);
    }
    section("memory cards (no saves, no images)", &memcard_summary(data));
    section("card backups (names and sizes only)", &backups(data));
    section("files in this report", &names.join(", "));
    let body = anon.apply(&body);

    let mut zip = zip::ZipWriter::new(
        std::fs::File::create(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?,
    );
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut add = |name: &str, bytes: &[u8]| -> Result<(), String> {
        zip.start_file(name, opts).map_err(|e| e.to_string())?;
        zip.write_all(bytes).map_err(|e| e.to_string())
    };
    let stripped = anon.stripped();
    let header = format!(
        "fepor-recomp bug report\nanonymised: {}\n",
        if stripped.is_empty() {
            "nothing needed it".to_string()
        } else {
            stripped.join(", ")
        }
    );
    add("report.txt", format!("{header}{body}").as_bytes())?;
    for (name, text) in &texts {
        add(name, text.as_bytes())?;
    }
    if let Some(png) = &screenshot {
        add("screenshot.png", png)?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(out.to_path_buf())
}

/// [`create_with`] for this machine's identity and the disc folder the game noted.
pub fn create(data: &Path, out: &Path, extra: Extra) -> Result<PathBuf, String> {
    let disc = Some(read_session(data).disc_dir).filter(|d| !d.is_empty());
    create_with(
        data,
        out,
        extra,
        &Anonymiser::new(Identity::from_env(), data).with_disc_dir(disc),
    )
}

/// `<data>/bug-reports/fepor-bug-report-<unix time>.zip`, the folder created.
pub fn default_output(data: &Path) -> Result<PathBuf, String> {
    let dir = data.join("bug-reports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(dir.join(format!("fepor-bug-report-{secs}.zip")))
}

/// Make the bundle in the default place.
pub fn create_default(data: &Path, extra: Extra) -> Result<PathBuf, String> {
    create(data, &default_output(data)?, extra)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn anon(data: &Path) -> Anonymiser {
        Anonymiser::new(
            Identity {
                homes: vec!["/home/bob".into()],
                user: Some("bob".into()),
                ..Default::default()
            },
            data,
        )
    }

    #[test]
    fn saves_game_data_and_mod_payloads_are_excluded() {
        for p in [
            "memcards/MemoryCardA.EU.raw",
            "memcards/backups/MemoryCardA.EU.100.raw",
            "imports/save.gci",
            "disc/sys/boot.bin",
            "disc/files/x.txt",
            "generated/a.cpp",
            "cheats/GFEP01.ini",
            "MemoryCardA.raw",
            "backup/slot.GCI",
            "main.dol",
            "imports\\a.gci.imported",
            "mods/x/files/a.txt",
            "mods/x/data.bin",
            "mods/x/readme.md",
        ] {
            assert!(is_excluded(p), "{p}");
        }
        for p in [
            "settings.toml",
            "input.toml",
            "mods/mods.toml",
            "mods/x/mod.toml",
            "cheats.toml",
            "playtime.toml",
            "logs/fepor-game.log",
            "logs/crash.txt",
            "screenshot.png",
            "disc.log",
            "logs/session.toml",
        ] {
            assert!(!is_excluded(p), "{p}");
        }
    }

    #[test]
    fn the_crash_block_starts_at_the_fatal_line() {
        let log = "start\nfepor: fatal: trap at 80001234\n  r1=0x1\n    backtrace: 1 2\n";
        assert_eq!(
            extract_crash(log).unwrap(),
            "fepor: fatal: trap at 80001234\n  r1=0x1\n    backtrace: 1 2\n"
        );
        assert!(extract_crash("all fine\n").is_none());
    }

    #[test]
    fn a_long_log_keeps_its_end() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l.log");
        std::fs::write(&p, "aaaa\nbbbb\ncccc\n").unwrap();
        let t = read_tail(&p, 8).unwrap();
        assert!(
            t.starts_with("[first ") && t.ends_with("cccc\n") && !t.contains("aaaa"),
            "{t}"
        );
        assert_eq!(read_tail(&p, 100).unwrap(), "aaaa\nbbbb\ncccc\n");
    }

    #[test]
    fn bundle_has_everything_anonymised_and_nothing_private() {
        let d = tempfile::tempdir().unwrap();
        let data = d.path();
        let w = |rel: &str, body: &[u8]| {
            let p = data.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w("settings.toml", b"[audio]\nvolume = 1.0\n");
        w("input.toml", b"x = 1\n");
        w("playtime.toml", b"seconds = 5\n");
        w(
            "logs/fepor-game.log",
            b"loaded /home/bob/Games/x as bob\nfepor: fatal: trap at 80001234\n  r1=0x1\n",
        );
        w("logs/fepor-game.prev.log", b"old\n");
        w("logs/leak.raw", b"card");
        w("memcards/MemoryCardA.EU.raw", b"card");
        w("memcards/backups/MemoryCardA.EU.100.raw", b"secretcard");
        w(
            "mods/mods.toml",
            b"order = [\"x\", \"y\"]\nenabled = [\"x\"]\n",
        );
        w("mods/x/mod.toml", b"name = \"X\"\ngame_id = \"GFEP01\"\n");
        w("mods/x/data.bin", b"modfile");
        w("mods/y/mod.toml", b"name = \"Y\"\ngame_id = \"GFEP01\"\n");
        w("cheats.toml", b"[GFEP01]\nenabled = [\"a\"]\n");
        w("cheats/GFEP01.ini", b"code");
        w("disc/sys/boot.bin", b"disc");
        w("imports/a.gci", b"save");
        w("screenshots/a.png", b"\x89PNG");
        update_session(data, |s| {
            s.game_id = "GFEP01".into();
            s.sections
                .insert("gpu".into(), "adapter: Test GPU at /home/bob/x".into());
        });
        let out = data.join("r.zip");
        let extra = Extra {
            screenshot: Some(b"\x89PNGlive".to_vec()),
            sections: vec![("frames".into(), "fps 60".into())],
        };
        create_with(data, &out, extra, &anon(data)).unwrap();
        let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut names: Vec<String> = z.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "cheats.toml",
                "crash.txt",
                "input.toml",
                "logs/fepor-game.log",
                "logs/fepor-game.prev.log",
                "logs/session.toml",
                "mods/mods.toml",
                "mods/x/mod.toml",
                "playtime.toml",
                "report.txt",
                "screenshot.png",
                "settings.toml"
            ]
        );
        let mut read = |n: &str| {
            let mut s = Vec::new();
            z.by_name(n).unwrap().read_to_end(&mut s).unwrap();
            String::from_utf8_lossy(&s).into_owned()
        };
        assert_eq!(
            read("logs/fepor-game.log").lines().next().unwrap(),
            "loaded <path> as <user>"
        );
        assert!(read("crash.txt").contains("fepor: fatal: trap at 80001234"));
        assert_eq!(
            read("screenshot.png"),
            "\u{89}PNGlive".replace('\u{89}', "\u{fffd}")
        );
        let report = read("report.txt");
        for want in [
            "anonymised: ",
            "game ID: GFEP01",
            "adapter: Test GPU at <path>",
            "fps 60",
            "MemoryCardA.EU.100.raw (10 bytes)",
            "cannot read as a card image",
        ] {
            assert!(report.contains(want), "{want}\n{report}");
        }
        assert!(!report.contains("secretcard") && !report.contains("/home/bob"));
    }
}
