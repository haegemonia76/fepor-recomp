//! The bug report bundle (ticket #47, ADR 0047): one zip with what a developer needs to read a problem report
//! and nothing the player would not want to send: the log, `settings.toml`, `input.toml`, `mods/mods.toml`, `cheats.toml`, version and system
//! information, the newest screenshot. No saves, no memory cards, no game data. The home directory is
//! replaced by `~` in every text file. Shared by `fepor-recomp bug-report` and the overlay's Help page.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

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
    /// wgpu adapter name and graphics API.
    pub gpu: String,
    pub gpu_backend: String,
}

pub fn session_path(data: &Path) -> PathBuf {
    data.join("logs").join("session.toml")
}

/// Change the session record (read, edit, write). Failures are ignored: it only feeds bug reports.
pub fn update_session(data: &Path, edit: impl FnOnce(&mut Session)) {
    let path = session_path(data);
    let mut s = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default();
    edit(&mut s);
    if let (Some(dir), Ok(text)) = (path.parent(), toml::to_string(&s)) {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&path, text);
    }
}

/// The version string of this build.
pub fn build_string() -> String {
    format!(
        "{} ({}, {}-{})",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) { "debug" } else { "release" },
        std::env::consts::ARCH,
        std::env::consts::OS
    )
}

/// Folders and file types that hold saves or game data: never in a bundle, whatever asks for them.
const PRIVATE_DIRS: &[&str] = &["memcards", "imports", "disc", "generated", "saves", "exports"];
const PRIVATE_EXTENSIONS: &[&str] = &[
    "raw", "gci", "sav", "gcm", "iso", "rvz", "gcz", "dol", "bin", "thp", "imported", "failed",
];

/// Is `rel` (a path relative to the data folder, either separator) a save or a piece of game data?
pub fn is_excluded(rel: &str) -> bool {
    let rel = rel.to_ascii_lowercase().replace('\\', "/");
    let mut parts = rel.split('/').filter(|p| !p.is_empty()).peekable();
    while let Some(p) = parts.next() {
        if parts.peek().is_some() && PRIVATE_DIRS.contains(&p) {
            return true;
        }
        if parts.peek().is_none() {
            return p
                .rsplit_once('.')
                .is_some_and(|(_, ext)| PRIVATE_EXTENSIONS.contains(&ext));
        }
    }
    false
}

/// The user's home directory as the environment states it, plus its resolved form (`/var/home/x` on
/// immutable distributions, where `/home` is a link).
pub fn home_dirs() -> Vec<String> {
    let mut homes = Vec::new();
    for var in ["HOME", "USERPROFILE"] {
        if let Some(h) = std::env::var_os(var).filter(|h| !h.is_empty()) {
            let h = PathBuf::from(h);
            if let Ok(real) = h.canonicalize() {
                homes.push(real.to_string_lossy().into_owned());
            }
            homes.push(h.to_string_lossy().into_owned());
        }
    }
    homes.dedup();
    homes
}

/// Replace each home directory in `text` with `~`, in the spellings a path takes in a log or a TOML file
/// (`/` or `\` separators, `\\` escaped). A longer name that merely starts like the home (`/home/bobby` for
/// `/home/bob`) is left alone.
pub fn strip_home(text: &str, homes: &[String]) -> String {
    let mut forms: Vec<String> = Vec::new();
    for h in homes {
        let h = h.trim_end_matches(['/', '\\']);
        if h.len() < 2 {
            continue;
        }
        let back = h.replace('/', "\\");
        forms.push(h.replace('\\', "/"));
        forms.push(back.replace('\\', "\\\\"));
        forms.push(back);
    }
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    forms.dedup();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'scan: while !rest.is_empty() {
        for f in &forms {
            if let Some(after) = rest.strip_prefix(f.as_str()) {
                let more = after
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
                if !more {
                    out.push('~');
                    rest = after;
                    continue 'scan;
                }
            }
        }
        let n = rest.chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[..n]);
        rest = &rest[n..];
    }
    out
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

/// File names and sizes in `<data>/memcards/backups`: the card images themselves are never read.
fn backups(data: &Path) -> String {
    let Ok(dir) = std::fs::read_dir(crate::memcard::backups_dir(data)) else {
        return "none".into();
    };
    let mut v: Vec<String> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| Some(format!("{} ({} bytes)", e.file_name().to_string_lossy(), e.metadata().ok()?.len())))
        .collect();
    v.sort();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

/// `report.txt`: versions, system and the list of files in the bundle.
fn report(data: &Path, files: &[String]) -> String {
    let s: Session = std::fs::read_to_string(session_path(data))
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default();
    let or = |v: &str| if v.is_empty() { "unknown (the game has not run here)".to_string() } else { v.to_string() };
    let mut t = String::new();
    let mut line = |k: &str, v: String| t.push_str(&format!("{k}: {v}\n"));
    line("report tool", build_string());
    line("game build", or(&s.build));
    line("engine", or(&s.engine));
    line("game ID", or(&s.game_id));
    line("GPU adapter", or(&s.gpu));
    line("graphics API", or(&s.gpu_backend));
    line("trace (FEPOR_TRACE)", if s.trace.is_empty() { "off".into() } else { s.trace.clone() });
    line("OS", format!("{} ({}, {})", os_name(), std::env::consts::OS, std::env::consts::ARCH));
    line("data folder", data.display().to_string());
    line("files", files.join(", "));
    line("card backups (names and sizes only)", backups(data));
    t
}

/// Files of `<data>/logs` worth sending: the logs themselves and the session record.
fn log_files(data: &Path) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(data.join("logs")) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = dir
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "log" || x == "txt"))
        .collect();
    v.sort();
    v
}

/// The most recently written `.png` in `<data>/screenshots`.
fn newest_screenshot(data: &Path) -> Option<PathBuf> {
    std::fs::read_dir(data.join("screenshots"))
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("png")))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, p)| p)
}

/// Write the bundle for the data folder `data` into `out` (a `.zip` path) and return it. Text is read
/// lossily and has every `homes` entry stripped; a file that `is_excluded` never goes in.
pub fn create(data: &Path, out: &Path, homes: &[String]) -> Result<PathBuf, String> {
    let rel = |p: &Path| {
        p.strip_prefix(data)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    // (name in the zip, file, is text)
    let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();
    // the two small files that name the enabled mods and cheats; nothing else of either folder goes in
    for name in ["settings.toml", "input.toml", "mods/mods.toml", "cheats.toml"] {
        entries.push((name.into(), data.join(name), true));
    }
    for p in log_files(data).into_iter().chain([session_path(data)]) {
        entries.push((rel(&p), p, true));
    }
    if let Some(p) = newest_screenshot(data) {
        entries.push(("screenshot.png".into(), p, false));
    }
    entries.retain(|(name, path, _)| path.is_file() && !is_excluded(name));
    entries.dedup_by(|a, b| a.0 == b.0);

    let mut names: Vec<String> = entries.iter().map(|e| e.0.clone()).collect();
    names.insert(0, "report.txt".into());
    let mut zip = zip::ZipWriter::new(
        std::fs::File::create(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?,
    );
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut add = |name: &str, bytes: &[u8]| -> Result<(), String> {
        zip.start_file(name, opts).map_err(|e| e.to_string())?;
        zip.write_all(bytes).map_err(|e| e.to_string())
    };
    add("report.txt", strip_home(&report(data, &names), homes).as_bytes())?;
    for (name, path, text) in &entries {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if *text {
            add(name, strip_home(&String::from_utf8_lossy(&bytes), homes).as_bytes())?;
        } else {
            add(name, &bytes)?;
        }
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(out.to_path_buf())
}

/// `<data>/bug-reports/fepor-bug-report-<unix time>.zip`, the folder created.
pub fn default_output(data: &Path) -> Result<PathBuf, String> {
    let dir = data.join("bug-reports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    Ok(dir.join(format!("fepor-bug-report-{secs}.zip")))
}

/// Make the bundle in the default place.
pub fn create_default(data: &Path) -> Result<PathBuf, String> {
    create(data, &default_output(data)?, &home_dirs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn homes() -> Vec<String> {
        vec!["/home/bob".into()]
    }

    #[test]
    fn home_is_replaced_in_every_spelling() {
        let h = vec!["C:\\Users\\Bob".to_string(), "/home/bob".to_string()];
        assert_eq!(strip_home("open /home/bob/.local/x", &h), "open ~/.local/x");
        assert_eq!(strip_home("C:\\Users\\Bob\\data", &h), "~\\data");
        assert_eq!(strip_home("path = \"C:\\\\Users\\\\Bob\\\\d\"", &h), "path = \"~\\\\d\"");
        assert_eq!(strip_home("C:/Users/Bob/d", &h), "~/d");
        assert_eq!(strip_home("a /home/bob b /home/bob", &h), "a ~ b ~");
    }

    #[test]
    fn longer_names_and_other_text_stay() {
        assert_eq!(strip_home("/home/bobby/x /home/bob.old", &homes()), "/home/bobby/x /home/bob.old");
        assert_eq!(strip_home("/home/alice/x ü", &homes()), "/home/alice/x ü");
        assert_eq!(strip_home("/x", &["/".into(), "".into()]), "/x");
    }

    #[test]
    fn saves_and_game_data_are_excluded() {
        for p in [
            "memcards/MemoryCardA.EU.raw",
            "imports/save.gci",
            "disc/sys/boot.bin",
            "disc/files/x.txt",
            "generated/a.cpp",
            "MemoryCardA.raw",
            "backup/slot.GCI",
            "main.dol",
            "memcards/backups/MemoryCardA.EU.100.raw",
            "imports\\a.gci.imported",
        ] {
            assert!(is_excluded(p), "{p}");
        }
        for p in ["settings.toml", "input.toml", "mods/mods.toml", "cheats.toml", "logs/fepor-game.log", "screenshot.png", "disc.log", "logs/session.toml"] {
            assert!(!is_excluded(p), "{p}");
        }
    }

    #[test]
    fn bundle_has_the_reports_files_stripped_and_nothing_private() {
        let d = tempfile::tempdir().unwrap();
        let data = d.path();
        let w = |rel: &str, body: &[u8]| {
            let p = data.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w("settings.toml", b"[audio]\nvolume = 1.0\n");
        w("input.toml", b"x = 1\n");
        w("logs/fepor-game.log", b"loaded /home/bob/Games/x\n");
        w("logs/fepor-game.1.log", b"old\n");
        w("logs/leak.raw", b"card");
        w("memcards/MemoryCardA.EU.raw", b"card");
        w("memcards/backups/MemoryCardA.EU.100.raw", b"secretcard");
        w("mods/mods.toml", b"[[mod]]\nname = \"x\"\nenabled = true\n");
        w("mods/x/mod.toml", b"name = \"x\"\n");
        w("mods/x/data.bin", b"modfile");
        w("cheats.toml", b"[GFEP01]\nenabled = [\"a\"]\n");
        w("cheats/GFEP01.ini", b"code");
        w("disc/sys/boot.bin", b"disc");
        w("imports/a.gci", b"save");
        w("screenshots/a.png", b"\x89PNG");
        update_session(data, |s| {
            s.gpu = "Test GPU".into();
            s.game_id = "GFEP01".into();
        });
        let out = data.join("r.zip");
        create(data, &out, &homes()).unwrap();
        let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut names: Vec<String> = z.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "cheats.toml",
                "input.toml",
                "logs/fepor-game.1.log",
                "logs/fepor-game.log",
                "logs/session.toml",
                "mods/mods.toml",
                "report.txt",
                "screenshot.png",
                "settings.toml"
            ]
        );
        let mut text = String::new();
        z.by_name("logs/fepor-game.log").unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "loaded ~/Games/x\n");
        text.clear();
        z.by_name("report.txt").unwrap().read_to_string(&mut text).unwrap();
        assert!(text.contains("MemoryCardA.EU.100.raw (10 bytes)"), "{text}");
        assert!(!text.contains("secretcard"));
        assert!(text.contains("GPU adapter: Test GPU") && text.contains("game ID: GFEP01"), "{text}");
    }
}
