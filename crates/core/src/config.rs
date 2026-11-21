use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Shared configuration read by the CLI and the GUI (TOML).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Disc image (ISO/GCM) or extracted folder (`sys/` + `files/`).
    pub disc: Option<PathBuf>,
    /// Output directory for extracted and generated data (git-ignored).
    pub output: Option<PathBuf>,
    /// Manual corrections for the static analysis.
    pub analysis: AnalysisConfig,
    pub generate: GenerateConfig,
}

/// Overrides for function discovery. Addresses are guest addresses (TOML hex literals work).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AnalysisConfig {
    /// Extra function starts.
    pub functions: Vec<u32>,
    /// Addresses that must never become function starts (rejects false positives).
    pub not_functions: Vec<u32>,
    /// Code ranges `[start, end)` that are data and must not be decoded.
    pub exclude: Vec<AddrRange>,
    /// Manually described jump tables.
    pub jump_tables: Vec<JumpTableOverride>,
    /// Guest functions replaced by runtime implementations (`fepor_hle_<name>`).
    pub hle: Vec<HleEntry>,
}

/// A guest function replaced by `fepor_hle_<name>(Context*)` in the runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HleEntry {
    pub addr: u32,
    pub name: String,
    /// The replacement may context-switch away and later be resumed: after each call the generated code
    /// checks `c.spr[1023]` (1 = unwind the host stack, see `fepor::unwind`).
    pub unwinds: bool,
    /// Calls to this function inside a function that also calls `resumes_at` (an HLE `setjmp`-like
    /// save function) continue at the instruction after that save call when the thread is resumed.
    pub resumes_at: Option<u32>,
}

impl AnalysisConfig {
    /// Append `defaults` after the user's entries (user entries win where order matters).
    pub fn with_defaults(mut self, defaults: AnalysisConfig) -> Self {
        self.functions.extend(defaults.functions);
        self.not_functions.extend(defaults.not_functions);
        self.exclude.extend(defaults.exclude);
        self.jump_tables.extend(defaults.jump_tables);
        self.hle.extend(defaults.hle);
        self
    }
}

/// Options for the C++ generator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct GenerateConfig {
    /// Functions per generated compilation unit.
    pub functions_per_unit: usize,
    /// Runtime sources (CMake project); defaults to the `runtime/` directory of this repository.
    pub runtime_dir: Option<PathBuf>,
    /// Emit a call-trace hook at every function entry (development builds).
    pub trace_calls: bool,
}

impl Default for GenerateConfig {
    fn default() -> Self {
        GenerateConfig {
            functions_per_unit: 256,
            runtime_dir: None,
            trace_calls: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AddrRange {
    pub start: u32,
    pub end: u32,
}

/// A jump table for the `bctr` at `branch`: either explicit `targets`, or `table` + `count` read from the image.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct JumpTableOverride {
    pub branch: u32,
    pub table: Option<u32>,
    pub count: Option<u32>,
    pub targets: Vec<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("invalid config {0}: {1}")]
    Parse(PathBuf, toml::de::Error),
    #[error("cannot serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io(path.into(), e))?;
        toml::from_str(&text).map_err(|e| ConfigError::Parse(path.into(), e))
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text).map_err(|e| ConfigError::Io(path.into(), e))
    }
}
