//! Player settings: video and audio options edited in the in-game overlay (ADR 0020) and kept in
//! `<data>/settings.toml`. Input bindings live in `input.toml` (crate `fepor-input`).
//!
//! A [`SettingsHandle`] is shared by the overlay (which edits), the renderer and the audio thread (which
//! read): readers compare `generation()` with the last value they saw to notice a change cheaply.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// How the 4:3 frame is fitted into the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aspect {
    /// Letterboxed at the frame's own 4:3.
    #[default]
    Fit,
    /// Fills the window.
    Stretch,
    /// The 4:3 picture stretched into a 16:9 box.
    Wide16x9,
    /// The 4:3 picture stretched into a 21:9 box.
    Wide21x9,
}

impl Aspect {
    pub const ALL: [Aspect; 4] = [
        Aspect::Fit,
        Aspect::Stretch,
        Aspect::Wide16x9,
        Aspect::Wide21x9,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Aspect::Fit => "Original 4:3",
            Aspect::Stretch => "Fill the window",
            Aspect::Wide16x9 => "Stretched to 16:9",
            Aspect::Wide21x9 => "Stretched to 21:9",
        }
    }

    /// Parse the `FEPOR_ASPECT` environment syntax (`fit`, `stretch`, `16:9`, `21:9`).
    pub fn parse(s: &str) -> Option<Aspect> {
        match s {
            "" | "fit" => Some(Aspect::Fit),
            "stretch" => Some(Aspect::Stretch),
            "16:9" => Some(Aspect::Wide16x9),
            "21:9" => Some(Aspect::Wide21x9),
            _ => None,
        }
    }
}

/// How much of the performance statistics is drawn over the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatsMode {
    #[default]
    Off,
    /// Just the frame rate.
    Fps,
    /// Frame times, CPU, GPU, audio and the rest.
    Full,
}

impl StatsMode {
    pub const ALL: [StatsMode; 3] = [StatsMode::Off, StatsMode::Fps, StatsMode::Full];

    pub fn label(self) -> &'static str {
        match self {
            StatsMode::Off => "Off",
            StatsMode::Fps => "Frame rate only",
            StatsMode::Full => "Full statistics",
        }
    }

    /// Off, frame rate, full, off...
    pub fn next(self) -> StatsMode {
        match self {
            StatsMode::Off => StatsMode::Fps,
            StatsMode::Fps => StatsMode::Full,
            StatsMode::Full => StatsMode::Off,
        }
    }
}

/// The phosphor mask of a CRT, drawn over the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mask {
    #[default]
    Off,
    /// Vertical red, green and blue stripes (aperture grille).
    Grille,
    /// Red, green and blue dots in staggered rows (shadow mask).
    Dots,
}

impl Mask {
    pub const ALL: [Mask; 3] = [Mask::Off, Mask::Grille, Mask::Dots];

    pub fn label(self) -> &'static str {
        match self {
            Mask::Off => "Off",
            Mask::Grille => "Stripes (aperture grille)",
            Mask::Dots => "Dots (shadow mask)",
        }
    }
}

/// Post-processing of the presented image (ADR 0039). The defaults leave the picture untouched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    /// Darkness of the gaps between the game's scanlines, 0.0 (none) ..= 1.0 (black).
    pub scanlines: f32,
    /// Display gamma: above 1.0 brightens the mid-tones, below darkens them. 0.5..=2.0.
    pub gamma: f32,
    /// 0.0 (grey) ..= 2.0; 1.0 is the original colours.
    pub saturation: f32,
    /// 0.5 ..= 1.5; 1.0 is the original contrast.
    pub contrast: f32,
    /// -0.5 ..= 0.5; 0.0 is the original brightness.
    pub brightness: f32,
    pub mask: Mask,
    /// How strongly the mask darkens the two colours a sub-pixel does not show, 0.0 ..= 1.0.
    pub mask_strength: f32,
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            scanlines: 0.0,
            gamma: 1.0,
            saturation: 1.0,
            contrast: 1.0,
            brightness: 0.0,
            mask: Mask::Off,
            mask_strength: 0.5,
        }
    }
}

impl Filters {
    /// Whether the picture is shown exactly as the game drew it.
    pub fn is_identity(&self) -> bool {
        self.scanlines <= 0.0
            && self.gamma == 1.0
            && self.saturation == 1.0
            && self.contrast == 1.0
            && self.brightness == 0.0
            && (self.mask == Mask::Off || self.mask_strength <= 0.0)
    }

    pub fn clamp(&mut self) {
        let fix = |v: &mut f32, lo: f32, hi: f32, default: f32| {
            *v = if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            };
        };
        fix(&mut self.scanlines, 0.0, 1.0, 0.0);
        fix(&mut self.gamma, 0.5, 2.0, 1.0);
        fix(&mut self.saturation, 0.0, 2.0, 1.0);
        fix(&mut self.contrast, 0.5, 1.5, 1.0);
        fix(&mut self.brightness, -0.5, 0.5, 0.0);
        fix(&mut self.mask_strength, 0.0, 1.0, 0.5);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Video {
    /// Images shown per game frame (1 = as the game draws them; more are interpolated). 1..=8.
    pub frame_multiplier: u32,
    /// Internal resolution factor over the GameCube's 640x528. 1..=4.
    pub resolution: u32,
    pub aspect: Aspect,
    pub fullscreen: bool,
    /// Wait for the display's refresh (no tearing). Off presents at once: lower latency, tearing possible.
    pub vsync: bool,
    /// The performance statistics shown in a corner of the window (F2 cycles).
    pub stats: StatsMode,
    /// Size of the overlay's text and widgets: 0 follows the window height (1x at 720 pixels), otherwise a
    /// multiple of the display's scale factor.
    pub overlay_scale: f32,
    /// Scanlines, colour correction and mask applied to the presented image (not to the overlay).
    pub filters: Filters,
}

impl Default for Video {
    fn default() -> Self {
        Video {
            frame_multiplier: 1,
            resolution: 1,
            aspect: Aspect::Fit,
            fullscreen: false,
            vsync: true,
            stats: StatsMode::Off,
            overlay_scale: 0.0,
            filters: Filters::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Master volume, 0.0..=1.0.
    pub volume: f32,
    /// Name of the playback device; `None` follows the system default.
    pub device: Option<String>,
}

impl Default for Audio {
    fn default() -> Self {
        Audio {
            volume: 1.0,
            device: None,
        }
    }
}

/// The language of the game's text, windows and menus. The game reads it from the console's SRAM
/// (`OSGetLanguage`) when it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    English,
    German,
    French,
    Spanish,
    Italian,
}

impl Language {
    pub const ALL: [Language; 5] = [
        Language::English,
        Language::German,
        Language::French,
        Language::Spanish,
        Language::Italian,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::German => "Deutsch",
            Language::French => "Français",
            Language::Spanish => "Español",
            Language::Italian => "Italiano",
        }
    }

    /// The value of the SRAM language byte (`OS_LANG_*`).
    pub fn sram_code(self) -> u8 {
        match self {
            Language::English => 0,
            Language::German => 1,
            Language::French => 2,
            Language::Spanish => 3,
            Language::Italian => 4,
        }
    }

    /// The ISO 639-1 code.
    pub fn code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::German => "de",
            Language::French => "fr",
            Language::Spanish => "es",
            Language::Italian => "it",
        }
    }

    /// Parse the `FEPOR_LANG` environment syntax: the two-letter code or the English name.
    pub fn parse(s: &str) -> Option<Language> {
        Language::ALL
            .into_iter()
            .find(|l| s.eq_ignore_ascii_case(l.code()) || s.eq_ignore_ascii_case(&format!("{l:?}")))
    }
}

/// Options of the game itself, applied when it starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Game {
    pub language: Language,
    /// Backup copies of the memory card kept (ticket #37); 0 turns the backups off.
    pub card_backups: u32,
}

impl Default for Game {
    fn default() -> Game {
        Game {
            language: Language::default(),
            card_backups: 5,
        }
    }
}

/// Discord rich presence (ADR 0045). Off unless the player turns it on: until then nothing is sent and no
/// socket is opened.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Presence {
    pub enabled: bool,
    /// The application id of the Discord application whose name and pictures are shown. Discord needs one
    /// to accept an activity; the repository ships none (see ADR 0045), so presence does nothing until it
    /// is set here.
    pub client_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub video: Video,
    pub audio: Audio,
    pub game: Game,
    pub presence: Presence,
    /// Developer tools in the overlay (the memory watcher, ADR 0048). `FEPOR_DEVELOPER=1` turns it on for a run.
    pub developer: bool,
}

impl Settings {
    /// Bring every value into its valid range.
    pub fn clamp(&mut self) {
        self.video.frame_multiplier = self.video.frame_multiplier.clamp(1, 8);
        self.video.resolution = self.video.resolution.clamp(1, 4);
        self.video.overlay_scale = self.video.overlay_scale.clamp(0.0, 4.0);
        self.video.filters.clamp();
        self.audio.volume = self.audio.volume.clamp(0.0, 1.0);
        self.game.card_backups = self.game.card_backups.min(50);
    }

    /// Load `path`; the defaults when it does not exist or cannot be read.
    pub fn load(path: &Path) -> Settings {
        let mut s: Settings = match std::fs::read_to_string(path) {
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                eprintln!("settings: {}: {e}; using the defaults", path.display());
                Settings::default()
            }),
            Err(_) => Settings::default(),
        };
        s.clamp();
        s
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(
            path,
            toml::to_string_pretty(self).map_err(std::io::Error::other)?,
        )
    }

    /// The environment overrides the debugging tools have always had (`FEPOR_FPS_MULT`, `FEPOR_RES`,
    /// `FEPOR_ASPECT`, `FEPOR_STATS`). They win over the file for this run and are not saved.
    pub fn with_env_overrides(mut self) -> Settings {
        let num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok());
        if std::env::var("FEPOR_DEVELOPER").is_ok_and(|v| v != "0") {
            self.developer = true;
        }
        if let Some(v) = std::env::var("FEPOR_STATS").ok() {
            self.video.stats = if v == "fps" {
                StatsMode::Fps
            } else {
                StatsMode::Full
            };
        }
        if let Some(n) = num("FEPOR_FPS_MULT") {
            self.video.frame_multiplier = n;
        }
        if let Some(n) = num("FEPOR_RES") {
            self.video.resolution = n;
        }
        if let Some(a) = std::env::var("FEPOR_ASPECT")
            .ok()
            .and_then(|v| Aspect::parse(&v))
        {
            self.video.aspect = a;
        }
        // started from the launcher's Big Picture mode: full screen, for this run
        if std::env::var("RECOMPLAUNCHER_MODE").is_ok_and(|m| m == "bigpicture") {
            self.video.fullscreen = true;
        }
        if let Some(l) = std::env::var("FEPOR_LANG")
            .ok()
            .and_then(|v| Language::parse(&v))
        {
            self.game.language = l;
        }
        self.clamp();
        self
    }
}

/// The settings file of a data folder.
pub fn path_in(data: &Path) -> PathBuf {
    data.join("settings.toml")
}

/// Copy into `file` every value that differs between `before` and `after`. A value that `after` lacks
/// (an `Option` set back to `None` is left out of the TOML) is removed from `file`.
fn apply_changes(file: &mut toml::Value, before: &toml::Value, after: &toml::Value) {
    let (Some(f), Some(b), Some(a)) = (file.as_table_mut(), before.as_table(), after.as_table())
    else {
        return;
    };
    for (k, av) in a {
        match (b.get(k), f.get_mut(k)) {
            (Some(bv), Some(fv)) if av.is_table() => apply_changes(fv, bv, av),
            (Some(bv), _) if bv == av => {}
            _ => {
                f.insert(k.clone(), av.clone());
            }
        }
    }
    for k in b.keys().filter(|k| !a.contains_key(*k)) {
        f.remove(k);
    }
}

/// Settings shared between the overlay (writer) and the renderer / audio thread (readers).
#[derive(Clone)]
pub struct SettingsHandle {
    inner: Arc<Mutex<Settings>>,
    /// The settings as the file holds them.
    file: Arc<Mutex<Settings>>,
    generation: Arc<AtomicU64>,
    path: Arc<PathBuf>,
}

impl SettingsHandle {
    pub fn new(settings: Settings, path: PathBuf) -> SettingsHandle {
        SettingsHandle::with_file(settings.clone(), settings, path)
    }

    /// `file` is what `path` holds, `effective` what this run uses (the file plus overrides).
    pub fn with_file(file: Settings, effective: Settings, path: PathBuf) -> SettingsHandle {
        SettingsHandle {
            inner: Arc::new(Mutex::new(effective)),
            file: Arc::new(Mutex::new(file)),
            generation: Arc::new(AtomicU64::new(1)),
            path: Arc::new(path),
        }
    }

    /// Loaded from `<data>/settings.toml` with the environment overrides applied (for this run only: they
    /// are not written back).
    pub fn load(data: &Path) -> SettingsHandle {
        let path = path_in(data);
        let file = Settings::load(&path);
        SettingsHandle::with_file(file.clone(), file.with_env_overrides(), path)
    }

    pub fn get(&self) -> Settings {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Bumped by every `update`: a reader that saw the previous value has something new to apply.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Edit the settings; saved to disk when the edit changed something.
    pub fn update(&self, edit: impl FnOnce(&mut Settings)) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = g.clone();
        edit(&mut g);
        g.clamp();
        if *g != before {
            // what the player changed goes into the file; what only an environment override set does not
            let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
            if let (Ok(mut f), Ok(b), Ok(a)) = (
                toml::Value::try_from(&*file),
                toml::Value::try_from(&before),
                toml::Value::try_from(&*g),
            ) {
                apply_changes(&mut f, &b, &a);
                if let Ok(s) = f.try_into::<Settings>() {
                    *file = s;
                }
            }
            let _ = file.save(&self.path);
            self.generation.fetch_add(1, Ordering::Release);
        }
    }
}

impl Default for SettingsHandle {
    /// Defaults, never written to disk (tests, tools).
    fn default() -> Self {
        SettingsHandle::new(Settings::default(), PathBuf::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_clamps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut s = Settings::default();
        s.video.frame_multiplier = 3;
        s.video.aspect = Aspect::Wide16x9;
        s.video.filters.scanlines = 0.25;
        s.video.filters.mask = Mask::Dots;
        s.audio.volume = 0.4;
        s.game.language = Language::Italian;
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        std::fs::write(
            &path,
            "[video]\nframe_multiplier = 99\nresolution = 0\n[audio]\nvolume = 7.0\n",
        )
        .unwrap();
        let l = Settings::load(&path);
        assert_eq!(
            (l.video.frame_multiplier, l.video.resolution, l.audio.volume),
            (8, 1, 1.0)
        );
        assert!(Settings::default().video.filters.is_identity());
        std::fs::write(&path, "[video.filters]\ngamma = 9.0\nscanlines = -1.0\n").unwrap();
        let f = Settings::load(&path).video.filters;
        assert_eq!((f.gamma, f.scanlines), (2.0, 0.0));
    }

    #[test]
    fn language_codes_and_names_parse_and_map_to_distinct_sram_values() {
        assert_eq!(Language::parse("de"), Some(Language::German));
        assert_eq!(Language::parse("French"), Some(Language::French));
        assert_eq!(Language::parse("xx"), None);
        let mut codes: Vec<u8> = Language::ALL.iter().map(|l| l.sram_code()).collect();
        codes.dedup();
        assert_eq!(codes.len(), Language::ALL.len());
        for l in Language::ALL {
            assert_eq!(Language::parse(l.code()), Some(l));
        }
    }

    #[test]
    fn handle_bumps_generation_only_on_change_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let h = SettingsHandle::new(Settings::default(), path.clone());
        let g0 = h.generation();
        h.update(|_| {});
        assert_eq!(h.generation(), g0);
        h.update(|s| s.video.resolution = 3);
        assert_eq!(h.generation(), g0 + 1);
        assert_eq!(Settings::load(&path).video.resolution, 3);
    }

    #[test]
    fn going_back_to_the_system_audio_device_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let h = SettingsHandle::new(Settings::default(), path.clone());
        h.update(|s| s.audio.device = Some("Soundbar".into()));
        assert_eq!(
            Settings::load(&path).audio.device.as_deref(),
            Some("Soundbar")
        );
        h.update(|s| s.audio.device = None);
        assert_eq!(Settings::load(&path).audio.device, None);
    }

    #[test]
    fn values_forced_for_one_run_are_not_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let file = Settings::default();
        let mut forced = file.clone();
        forced.video.fullscreen = true;
        forced.video.frame_multiplier = 4;
        let h = SettingsHandle::with_file(file, forced, path.clone());
        h.update(|s| s.audio.volume = 0.5);
        let saved = Settings::load(&path);
        assert_eq!(saved.audio.volume, 0.5);
        assert!(!saved.video.fullscreen);
        assert_eq!(saved.video.frame_multiplier, 1);
        // the run itself keeps what was forced
        assert!(h.get().video.fullscreen);
        // a change the player makes to a forced value does reach the file
        h.update(|s| s.video.frame_multiplier = 2);
        assert_eq!(Settings::load(&path).video.frame_multiplier, 2);
    }
}
