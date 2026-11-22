//! `fepor-recomp run`: start the built game the way RecompLauncher does (profile `start`).
//!
//! The game is `fepor-game` next to this executable (a release archive) or on the PATH. It gets the data
//! folder and, when given, the player's copy; RecompLauncher's `RECOMPLAUNCHER_MODE` and
//! `RECOMPLAUNCHER_PAD` reach it through the environment. Everything it prints is kept in
//! `<data>/logs/fepor-game.log` (the last few runs as `.1.log`, `.2.log`, ...), which RecompLauncher sends
//! with a crash report. Nothing here updates anything. The exit code is the game's.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::session::{Fail, Options};

/// How many earlier logs are kept.
const KEEP: usize = 4;

fn game_program() -> PathBuf {
    let name = if cfg!(windows) {
        "fepor-game.exe"
    } else {
        "fepor-game"
    };
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Shift `fepor-game.log` to `.1.log` and so on, dropping the oldest.
fn rotate(dir: &Path) {
    let name = |n: usize| {
        dir.join(if n == 0 {
            "fepor-game.log".to_string()
        } else {
            format!("fepor-game.{n}.log")
        })
    };
    let _ = std::fs::remove_file(name(KEEP));
    for n in (0..KEEP).rev() {
        let _ = std::fs::rename(name(n), name(n + 1));
    }
}

pub fn run(opts: &Options) -> Result<(), Fail> {
    let data = opts.output()?;
    let logs = data.join("logs");
    std::fs::create_dir_all(&logs)
        .map_err(|e| Fail::Other(format!("cannot create {}: {e}", logs.display())))?;
    rotate(&logs);
    let log_path = logs.join("fepor-game.log");
    let log = Arc::new(Mutex::new(File::create(&log_path).map_err(|e| {
        Fail::Other(format!("cannot create {}: {e}", log_path.display()))
    })?));

    let program = game_program();
    let mut cmd = Command::new(&program);
    cmd.arg("--data").arg(data);
    if let Ok(game) = opts.disc() {
        cmd.arg("--game").arg(game);
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Fail::Other(format!("cannot start {}: {e}", program.display())))?;

    let tee = |stream: Box<dyn std::io::Read + Send>, to_stderr: bool| {
        let log = log.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if to_stderr {
                    eprintln!("{line}");
                } else {
                    println!("{line}");
                }
                if let Ok(mut f) = log.lock() {
                    let _ = writeln!(f, "{line}");
                }
            }
        })
    };
    let out = tee(Box::new(child.stdout.take().expect("piped stdout")), false);
    let err = tee(Box::new(child.stderr.take().expect("piped stderr")), true);
    let status = child
        .wait()
        .map_err(|e| Fail::Other(format!("waiting for the game: {e}")))?;
    let _ = out.join();
    let _ = err.join();
    match status.code() {
        Some(0) => Ok(()),
        Some(c) => Err(Fail::Game(
            c,
            format!(
                "the game exited with code {c} (log: {})",
                log_path.display()
            ),
        )),
        None => Err(Fail::Game(
            1,
            format!(
                "the game was stopped by a signal (log: {})",
                log_path.display()
            ),
        )),
    }
}
