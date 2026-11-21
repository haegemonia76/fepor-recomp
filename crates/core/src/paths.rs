//! Where the front ends keep their settings and where a finished recompilation can be found.

use std::path::{Path, PathBuf};

/// `<config dir>/fepor-recomp/gui.toml`: the launcher GUI's remembered game file and output folder.
pub fn gui_config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("fepor-recomp").join("gui.toml")
}

/// Has `prepare` finished in `dir`? (Its marker is written last.)
pub fn is_prepared(dir: &Path) -> bool {
    dir.join("prepared.json").exists()
}

/// The output folder the launcher GUI last used, when a finished recompilation is in it.
pub fn last_prepared_output() -> Option<PathBuf> {
    #[derive(serde::Deserialize)]
    struct Saved {
        output: String,
    }
    let text = std::fs::read_to_string(gui_config_path()).ok()?;
    let saved: Saved = toml::from_str(&text).ok()?;
    let dir = PathBuf::from(saved.output.trim());
    is_prepared(&dir).then_some(dir)
}

/// The folder holding the prepared game, saves and settings when nothing was said on the command line:
/// `RECOMPLAUNCHER_DATA`, else the launcher GUI's last output, else `$XDG_DATA_HOME/fepor-recomp`.
pub fn default_data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("RECOMPLAUNCHER_DATA").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    if let Some(d) = last_prepared_output() {
        return d;
    }
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("fepor-recomp")
}

/// The extracted game data `prepare` leaves in `data`, if there is one.
pub fn prepared_disc(data: &Path) -> Option<PathBuf> {
    let disc = data.join("disc");
    disc.join("sys/boot.bin").exists().then_some(disc)
}
