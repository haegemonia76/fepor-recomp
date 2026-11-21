//! State shared by all subcommands: resolved options, the opened disc, the verified DOL.

use std::path::PathBuf;

use fepor_core::config::AnalysisConfig;
use fepor_core::dol::Dol;
use fepor_core::{open_disc, version, Config, Disc};

use crate::Cli;

/// Failure categories. The exit code is stable so launchers can map it to a message
/// (`exit_codes` in recomplauncher.json).
#[derive(Debug)]
pub enum Fail {
    Usage(String),
    /// The disc image is missing, unreadable or not a GameCube disc.
    Disc(String),
    /// A valid disc of a game or version that is not supported.
    Version(String),
    /// cmake, ninja or clang++ is missing.
    Toolchain(String),
    Build(String),
    Other(String),
}

impl Fail {
    pub fn code(&self) -> u8 {
        match self {
            Fail::Other(_) => 1,
            Fail::Usage(_) => 2,
            Fail::Disc(_) => 10,
            Fail::Version(_) => 11,
            Fail::Toolchain(_) => 20,
            Fail::Build(_) => 30,
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Fail::Usage(m)
            | Fail::Disc(m)
            | Fail::Version(m)
            | Fail::Toolchain(m)
            | Fail::Build(m)
            | Fail::Other(m) => m,
        }
    }
}

pub struct Options {
    disc: Option<PathBuf>,
    output: Option<PathBuf>,
    pub no_verify: bool,
    pub cfg: Config,
}

impl Options {
    pub fn new(cli: &Cli, cfg: Config) -> Options {
        Options {
            disc: cli.disc.clone().or(cfg.disc.clone()),
            output: cli.output.clone().or(cfg.output.clone()),
            no_verify: cli.no_verify,
            cfg,
        }
    }

    pub fn disc(&self) -> Result<&PathBuf, Fail> {
        self.disc.as_ref().ok_or_else(|| {
            Fail::Usage("no disc given (use --disc/--game or `disc` in the config)".into())
        })
    }

    pub fn output(&self) -> Result<&PathBuf, Fail> {
        self.output.as_ref().ok_or_else(|| Fail::Usage("no output directory given (use --output/--data, RECOMPLAUNCHER_DATA or `output` in the config)".into()))
    }
}

pub struct Session {
    pub disc: Box<dyn Disc>,
    pub dol: Dol,
    pub analysis_cfg: AnalysisConfig,
}

impl Session {
    /// Open the disc, check the game ID and `main.dol` hash (unless `--no-verify`) and parse the DOL.
    pub fn open(opts: &Options) -> Result<Session, Fail> {
        let mut disc = open_disc(opts.disc()?).map_err(|e| Fail::Disc(e.to_string()))?;
        let bytes = disc.dol().map_err(|e| Fail::Disc(e.to_string()))?;
        if !opts.no_verify {
            let v =
                version::check(disc.game_id(), &bytes).map_err(|e| Fail::Version(e.to_string()))?;
            eprintln!("verified: {} ({})", v.name, v.game_id);
        }
        let dol = Dol::parse(bytes).map_err(|e| Fail::Disc(e.to_string()))?;
        let defaults = version::lookup(disc.game_id())
            .map(|v| v.analysis_defaults())
            .unwrap_or_default();
        Ok(Session {
            disc,
            dol,
            analysis_cfg: opts.cfg.analysis.clone().with_defaults(defaults),
        })
    }
}
