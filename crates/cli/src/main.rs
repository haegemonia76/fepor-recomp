use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use fepor_core::{analysis::analyze, dol::Dol, extract::extract, open_disc, version, Config};

#[derive(Parser)]
#[command(version, about = "Static recompilation of Fire Emblem: Path of Radiance")]
struct Cli {
    /// TOML config file; command-line options override it.
    #[arg(long, short, global = true, default_value = "fepor.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Extract main.dol, system files and the file system from a disc image.
    Extract {
        /// ISO/GCM file or extracted folder.
        #[arg(long)]
        disc: Option<PathBuf>,
        /// Output directory.
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Skip the game ID / main.dol hash check.
        #[arg(long)]
        no_verify: bool,
    },
    /// Generate C++ for the whole DOL into `<output>/generated`.
    Generate {
        /// ISO/GCM file or extracted folder.
        #[arg(long)]
        disc: Option<PathBuf>,
        /// Output directory.
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Functions per compilation unit (default from config, else 256).
        #[arg(long)]
        functions_per_unit: Option<usize>,
        /// Runtime CMake project (default: `runtime/` of this repository).
        #[arg(long)]
        runtime_dir: Option<PathBuf>,
        /// Skip the game ID / main.dol hash check.
        #[arg(long)]
        no_verify: bool,
    },
    /// Analyse main.dol: function starts, jump tables, unresolved indirect branches.
    Analyze {
        /// ISO/GCM file or extracted folder.
        #[arg(long)]
        disc: Option<PathBuf>,
        /// Output directory; the report goes to `<output>/analysis.txt` unless `--report` is given.
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Report file.
        #[arg(long)]
        report: Option<PathBuf>,
        /// Skip the game ID / main.dol hash check.
        #[arg(long)]
        no_verify: bool,
    },
}

fn load_config(path: &PathBuf) -> Result<Config, String> {
    if path.exists() {
        Config::load(path).map_err(|e| e.to_string())
    } else {
        Ok(Config::default())
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let mut cfg = load_config(&cli.config)?;
    match cli.command {
        Command::Extract { disc, output, no_verify } => {
            cfg.disc = disc.or(cfg.disc);
            cfg.output = output.or(cfg.output);
            let disc_path = cfg.disc.ok_or("no disc given (use --disc or `disc` in the config)")?;
            let out = cfg.output.ok_or("no output directory given (use --output or `output` in the config)")?;

            let mut disc = open_disc(&disc_path).map_err(|e| e.to_string())?;
            if !no_verify {
                let dol = disc.dol().map_err(|e| e.to_string())?;
                let v = version::check(disc.game_id(), &dol).map_err(|e| e.to_string())?;
                eprintln!("verified: {} ({})", v.name, v.game_id);
            }
            let report = extract(&mut *disc, &out, |_| {}).map_err(|e| e.to_string())?;
            eprintln!(
                "extracted {} system files and {} files ({} bytes) to {}",
                report.sys_files,
                report.files,
                report.bytes,
                out.display()
            );
            Ok(())
        }
        Command::Generate { disc, output, functions_per_unit, runtime_dir, no_verify } => {
            let disc_path = disc.or(cfg.disc.clone()).ok_or("no disc given (use --disc or `disc` in the config)")?;
            let out = output.or(cfg.output.clone()).ok_or("no output directory given (use --output or `output` in the config)")?;
            let mut disc = open_disc(&disc_path).map_err(|e| e.to_string())?;
            let bytes = disc.dol().map_err(|e| e.to_string())?;
            if !no_verify {
                let v = version::check(disc.game_id(), &bytes).map_err(|e| e.to_string())?;
                eprintln!("verified: {} ({})", v.name, v.game_id);
            }
            let dol = Dol::parse(bytes).map_err(|e| e.to_string())?;
            let defaults = version::lookup(disc.game_id()).map(|v| v.analysis_defaults()).unwrap_or_default();
            let analysis = analyze(&dol, &cfg.analysis.with_defaults(defaults));
            eprintln!("{}", analysis.summary());

            let runtime_dir = runtime_dir
                .or(cfg.generate.runtime_dir.clone())
                .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../runtime")));
            let runtime_dir = std::fs::canonicalize(&runtime_dir).map_err(|e| format!("runtime dir {}: {e}", runtime_dir.display()))?;
            let gen_dir = out.join("generated");
            std::fs::create_dir_all(&gen_dir).map_err(|e| format!("{}: {e}", gen_dir.display()))?;
            let opts = fepor_ppc_lift::Options {
                functions_per_unit: functions_per_unit.unwrap_or(cfg.generate.functions_per_unit),
                runtime_dir: runtime_dir.display().to_string(),
            };
            let stats = fepor_ppc_lift::generate(&dol, &analysis, &opts, &mut |name, content| std::fs::write(gen_dir.join(name), content))
                .map_err(|e| e.to_string())?;
            eprintln!("generated {} functions ({} instructions) in {} units under {}", stats.functions, stats.instructions, stats.units, gen_dir.display());
            let total: usize = stats.unimplemented.values().sum();
            eprintln!("{total} instructions emitted as runtime traps");
            for (op, n) in &stats.unimplemented {
                eprintln!("  {op}: {n}");
            }
            Ok(())
        }
        Command::Analyze { disc, output, report, no_verify } => {
            let disc_path = disc.or(cfg.disc).ok_or("no disc given (use --disc or `disc` in the config)")?;
            let mut disc = open_disc(&disc_path).map_err(|e| e.to_string())?;
            let bytes = disc.dol().map_err(|e| e.to_string())?;
            if !no_verify {
                let v = version::check(disc.game_id(), &bytes).map_err(|e| e.to_string())?;
                eprintln!("verified: {} ({})", v.name, v.game_id);
            }
            let dol = Dol::parse(bytes).map_err(|e| e.to_string())?;
            let defaults = version::lookup(disc.game_id()).map(|v| v.analysis_defaults()).unwrap_or_default();
            let analysis = analyze(&dol, &cfg.analysis.with_defaults(defaults));
            eprintln!("{}", analysis.summary());
            if let Some(path) = report.or(output.or(cfg.output).map(|o| o.join("analysis.txt"))) {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                }
                std::fs::write(&path, analysis.report()).map_err(|e| format!("{}: {e}", path.display()))?;
                eprintln!("report written to {}", path.display());
            }
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
