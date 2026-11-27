//! The game's own log (ticket #47, ADR 0047): stderr is mirrored into `<data>/logs/fepor-game.log`, the
//! previous run's log is kept as `fepor-game.prev.log`, and a log that grows past [`CAP`] starts over (so
//! the end, where a crash is, is what stays). A bug report always has a log, whoever started the game.
//!
//! `fepor-recomp run` keeps the log itself (and tells the game with `FEPOR_LOGGED=1`). The mirror needs
//! `dup2`, so it exists on Unix only; elsewhere the game writes no file of its own.

use std::path::Path;

/// A log is cut when it reaches this size (bytes).
pub const CAP: u64 = 16 << 20;

pub const LOG_NAME: &str = "fepor-game.log";
pub const PREV_NAME: &str = "fepor-game.prev.log";

/// Mirror stderr into the log file in `data/logs`. Does nothing when the launcher keeps the log
/// (`FEPOR_LOGGED`), when it is already installed, or off Unix.
pub fn install(data: &Path) {
    if std::env::var_os("FEPOR_LOGGED").is_some() {
        return;
    }
    #[cfg(unix)]
    unix::install(data);
    #[cfg(not(unix))]
    let _ = data;
}

/// End the process after the mirror copied what was written so far: `process::exit` would otherwise
/// drop the last lines (the crash report) still in the pipe.
pub fn exit(code: i32) -> ! {
    #[cfg(unix)]
    unix::drain();
    std::process::exit(code)
}

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::time::{Duration, Instant};

    use super::{CAP, LOG_NAME, PREV_NAME};

    static READ_FD: AtomicI32 = AtomicI32::new(-1);
    static BUSY: AtomicBool = AtomicBool::new(false);

    pub fn install(data: &Path) {
        if READ_FD.load(Ordering::Relaxed) >= 0 {
            return;
        }
        let dir = data.join("logs");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let (log, prev) = (dir.join(LOG_NAME), dir.join(PREV_NAME));
        let _ = std::fs::rename(&log, &prev);
        let Ok(file) = File::create(&log) else { return };
        let Ok((mut rx, tx)) = std::io::pipe() else {
            return;
        };
        // SAFETY: plain fd calls; the saved descriptor is owned by the `File` made from it, fd 2 is
        // replaced by the pipe's write end and the temporary write end is closed with `tx`.
        let console = unsafe {
            let saved = libc::dup(2);
            if saved < 0 || libc::dup2(tx.as_raw_fd(), 2) < 0 {
                return;
            }
            File::from_raw_fd(saved)
        };
        drop(tx);
        READ_FD.store(rx.as_raw_fd(), Ordering::Relaxed);
        let _ = std::thread::Builder::new()
            .name("log".into())
            .spawn(move || copy(&mut rx, console, file, &log));
    }

    fn copy(rx: &mut std::io::PipeReader, mut console: File, mut file: File, path: &Path) {
        let (mut buf, mut size) = (vec![0u8; 16 << 10], 0u64);
        loop {
            BUSY.store(false, Ordering::Release);
            let n = match rx.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            BUSY.store(true, Ordering::Release);
            let _ = console.write_all(&buf[..n]);
            if size + n as u64 > CAP {
                if let Ok(f) = File::create(path) {
                    file = f;
                    let _ = file.write_all(b"[log cut here: it reached its size limit]\n");
                    size = 0;
                }
            }
            if file.write_all(&buf[..n]).is_ok() {
                size += n as u64;
            }
        }
    }

    /// Wait (at most a second) until everything written to stderr has been copied.
    pub fn drain() {
        let fd = READ_FD.load(Ordering::Relaxed);
        if fd < 0 {
            return;
        }
        let (end, mut quiet) = (Instant::now() + Duration::from_secs(1), 0);
        while Instant::now() < end {
            let mut pending: libc::c_int = 0;
            // SAFETY: FIONREAD writes one int; `fd` is the pipe's read end, open for the process's life.
            let ok = unsafe { libc::ioctl(fd, libc::FIONREAD as _, &mut pending) } == 0;
            quiet = if pending == 0 && !BUSY.load(Ordering::Acquire) {
                quiet + 1
            } else {
                0
            };
            // twice in a row: the copier may be between `read` returning and marking itself busy
            if !ok || quiet >= 2 {
                return;
            }
            std::thread::sleep(Duration::from_millis(3));
        }
    }
}
