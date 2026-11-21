//! `fepor-recomp`: command-line front end. Every subcommand shares the same global options and
//! `Session` (disc, version check, DOL, analysis), so scripts and launchers drive one tool.

mod commands;
mod session;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use fepor_core::Config;

use session::Fail;

#[derive(Parser)]
#[command(
    version,
    about = "Static recompilation of Fire Emblem: Path of Radiance"
)]
pub struct Cli {
    /// TOML config file; command-line options override it.
    #[arg(long, short, global = true, default_value = "fepor.toml")]
    pub config: PathBuf,
    /// The player's copy: ISO/GCM file or extracted folder. (`--game` is the RecompLauncher spelling.)
    #[arg(long, global = true, alias = "game")]
    pub disc: Option<PathBuf>,
    /// Where everything the tool makes goes. RecompLauncher passes it as `--data` / RECOMPLAUNCHER_DATA.
    #[arg(
        long,
        short,
        global = true,
        alias = "data",
        env = "RECOMPLAUNCHER_DATA"
    )]
    pub output: Option<PathBuf>,
    /// Skip the game ID / main.dol hash check.
    #[arg(long, global = true)]
    pub no_verify: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Extract main.dol, system files and the file system from the disc image into `<output>`.
    Extract,
    /// Analyse main.dol: function starts, jump tables, unresolved indirect branches (`<output>/analysis.txt`).
    Analyze {
        /// Report file instead of `<output>/analysis.txt`.
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// Generate C++ for the whole DOL into `<output>/generated`.
    Generate(commands::generate::GenerateArgs),
    /// Generate and build the game from the player's copy, printing `progress` lines (RecompLauncher setup step).
    Prepare(commands::generate::GenerateArgs),
    /// Disassemble guest code from main.dol.
    Disasm {
        /// Guest address, e.g. 0x80003154.
        #[arg(long, value_parser = commands::inspect::parse_addr)]
        addr: u32,
        /// Number of instructions.
        #[arg(long, default_value_t = 16)]
        count: u32,
    },
    /// Instruction histogram of main.dol's text sections.
    Opcodes,
    /// List every instruction of an opcode (e.g. `--op Rfi`, `--op Sc`, `--op Mtmsr`) with its function.
    Find {
        /// ppc750cl opcode name, case-insensitive.
        #[arg(long, conflicts_with = "callers")]
        op: Option<String>,
        /// List the direct callers (`bl`/`b`) of this guest address instead.
        #[arg(long, value_parser = commands::inspect::parse_addr)]
        callers: Option<u32>,
        /// List data words equal to this address and `lis`+`addi/ori` pairs that load it (function pointers).
        #[arg(long, value_parser = commands::inspect::parse_addr, conflicts_with_all = ["op", "callers"])]
        pointers: Option<u32>,
    },
}

fn run(cli: Cli) -> Result<(), Fail> {
    let cfg = if cli.config.exists() {
        Config::load(&cli.config).map_err(|e| Fail::Usage(e.to_string()))?
    } else {
        Config::default()
    };
    let opts = session::Options::new(&cli, cfg);
    match cli.command {
        Command::Extract => commands::extract::run(&opts),
        Command::Analyze { report } => commands::analyze::run(&opts, report),
        Command::Generate(args) => commands::generate::run(&opts, &args, false),
        Command::Prepare(args) => commands::generate::run(&opts, &args, true),
        Command::Disasm { addr, count } => commands::inspect::disasm(&opts, addr, count),
        Command::Opcodes => commands::inspect::opcodes(&opts),
        Command::Find {
            op,
            callers,
            pointers,
        } => match pointers {
            Some(p) => commands::inspect::pointers(&opts, p),
            None => commands::inspect::find(&opts, op.as_deref(), callers),
        },
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {}", e.message());
            ExitCode::from(e.code())
        }
    }
}
