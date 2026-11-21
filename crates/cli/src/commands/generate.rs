use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::Args;
use fepor_core::analysis::analyze;

use crate::session::{Fail, Options, Session};

#[derive(Args)]
pub struct GenerateArgs {
    /// Functions per compilation unit (default from the config, else 256).
    #[arg(long)]
    pub functions_per_unit: Option<usize>,
    /// Runtime CMake project (default: `runtime/` of this repository).
    #[arg(long)]
    pub runtime_dir: Option<PathBuf>,
    /// Emit `fepor_enter(addr)` at every function entry (call tracing for development builds).
    #[arg(long)]
    pub trace_calls: bool,
    /// C++ compiler for `prepare`; must be clang.
    #[arg(long, default_value = "clang++", env = "CXX")]
    pub cxx: String,
    /// CMake build type for `prepare`.
    #[arg(long, default_value = "Release")]
    pub build_type: String,
}

/// RecompLauncher setup-step progress line: `progress <0..1> <text>`.
fn progress(fraction: f32, text: &str) {
    println!("progress {fraction:.2} {text}");
    let _ = std::io::stdout().flush();
}

/// Write `content` only when it differs, so unchanged units keep their timestamps and ninja rebuilds nothing.
fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    std::fs::write(path, content)
}

fn generate(opts: &Options, args: &GenerateArgs, session: &Session, out: &Path) -> Result<PathBuf, Fail> {
    let analysis = analyze(&session.dol, &session.analysis_cfg);
    eprintln!("{}", analysis.summary());

    let runtime_dir = args
        .runtime_dir
        .clone()
        .or(opts.cfg.generate.runtime_dir.clone())
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../runtime")));
    let runtime_dir = std::fs::canonicalize(&runtime_dir).map_err(|e| Fail::Usage(format!("runtime dir {}: {e}", runtime_dir.display())))?;
    let gen_dir = out.join("generated");
    std::fs::create_dir_all(&gen_dir).map_err(|e| Fail::Other(format!("{}: {e}", gen_dir.display())))?;

    let lift_opts = fepor_ppc_lift::Options {
        functions_per_unit: args.functions_per_unit.unwrap_or(opts.cfg.generate.functions_per_unit),
        runtime_dir: runtime_dir.display().to_string(),
        trace_calls: args.trace_calls || opts.cfg.generate.trace_calls,
        hle: session.analysis_cfg.hle.clone(),
    };
    let stats = fepor_ppc_lift::generate(&session.dol, &analysis, &lift_opts, &mut |name, content| write_if_changed(&gen_dir.join(name), content))
        .map_err(|e| Fail::Other(e.to_string()))?;
    eprintln!("generated {} functions ({} instructions) in {} units under {}", stats.functions, stats.instructions, stats.units, gen_dir.display());
    let total: usize = stats.unimplemented.values().sum();
    eprintln!("{total} instructions emitted as runtime traps");
    for (op, n) in &stats.unimplemented {
        eprintln!("  {op}: {n}");
    }
    Ok(gen_dir)
}

fn require_tool(tool: &str) -> Result<(), Fail> {
    match Command::new(tool).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status() {
        Ok(_) => Ok(()),
        Err(_) => Err(Fail::Toolchain(format!("{tool} was not found on PATH; install clang, cmake and ninja to build the game"))),
    }
}

/// Run a build tool, forwarding its output to stderr; `on_line` sees every stdout line.
fn run_tool(cmd: &mut Command, what: &str, mut on_line: impl FnMut(&str)) -> Result<(), Fail> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| Fail::Toolchain(format!("cannot start {what}: {e}")))?;
    let mut tail: Vec<String> = Vec::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        eprintln!("{line}");
        on_line(&line);
        tail.push(line);
        if tail.len() > 20 {
            tail.remove(0);
        }
    }
    let status = child.wait().map_err(|e| Fail::Build(format!("{what}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Fail::Build(format!("{what} failed ({status}); last output:\n{}", tail.join("\n"))))
    }
}

/// Ninja prints `[12/43] ...`; returns (done, total).
fn parse_ninja_progress(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix('[')?;
    let (counts, _) = rest.split_once(']')?;
    let (done, total) = counts.split_once('/')?;
    Some((done.trim().parse().ok()?, total.trim().parse().ok()?))
}

pub fn run(opts: &Options, args: &GenerateArgs, prepare: bool) -> Result<(), Fail> {
    let out = opts.output()?.clone();
    if prepare {
        for tool in ["cmake", "ninja", args.cxx.as_str()] {
            require_tool(tool)?;
        }
    }
    progress(0.02, "Reading the disc image");
    let session = Session::open(opts)?;
    progress(0.10, "Analysing the game code");
    let gen_dir = {
        progress(0.20, "Generating C++");
        generate(opts, args, &session, &out)?
    };
    if !prepare {
        return Ok(());
    }

    let build_dir = out.join("build");
    progress(0.30, "Configuring the build");
    run_tool(
        Command::new("cmake")
            .arg("-S")
            .arg(&gen_dir)
            .arg("-B")
            .arg(&build_dir)
            .args(["-G", "Ninja"])
            .arg(format!("-DCMAKE_CXX_COMPILER={}", args.cxx))
            .arg(format!("-DCMAKE_BUILD_TYPE={}", args.build_type)),
        "cmake configure",
        |_| {},
    )?;
    progress(0.35, "Building the game");
    run_tool(Command::new("cmake").arg("--build").arg(&build_dir), "cmake build", |line| {
        if let Some((done, total)) = parse_ninja_progress(line).filter(|&(_, t)| t > 0) {
            progress(0.35 + 0.63 * done as f32 / total as f32, &format!("Building the game ({done}/{total})"));
        }
    })?;
    // Marker for RecompLauncher's `prepare.done`: written last, so its presence means the build finished.
    let marker = format!(
        "{{\n  \"tool\": \"{}\",\n  \"game_id\": \"{}\",\n  \"build_type\": \"{}\"\n}}\n",
        env!("CARGO_PKG_VERSION"),
        session.disc.game_id(),
        args.build_type
    );
    std::fs::write(out.join("prepared.json"), marker).map_err(|e| Fail::Other(format!("prepared.json: {e}")))?;
    progress(1.0, "Done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_ninja_progress;

    #[test]
    fn ninja_progress_lines() {
        assert_eq!(parse_ninja_progress("[12/43] Building CXX object x.o"), Some((12, 43)));
        assert_eq!(parse_ninja_progress("[ 3/9] Linking"), Some((3, 9)));
        assert_eq!(parse_ninja_progress("warning: x"), None);
    }
}
