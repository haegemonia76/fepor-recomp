//! Discord rich presence (ADR 0045): a minimal client of Discord's local IPC, without a library.
//!
//! Strictly opt-in (`presence.enabled` in `settings.toml`, the overlay's Help page). While it is off the
//! thread only reads the setting: no socket is looked for, nothing is written anywhere. What is sent when it
//! is on: the application id (to connect), the process id, the start time of this run and one of two fixed
//! texts. Nothing is read from the guest, so no chapter or scene is shown.
//!
//! Protocol: frames of `u32 opcode, u32 length` (little endian) and a JSON body, over a Unix socket
//! `discord-ipc-N` (Linux, macOS) or the named pipe `\\?\pipe\discord-ipc-N` (Windows). The handshake
//! (opcode 0) is answered with a READY event; activities go in opcode-1 `SET_ACTIVITY` commands.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fepor_overlay::Shared;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
/// Discord accepts a handful of activity updates per 20 seconds.
const MIN_UPDATE_GAP: Duration = Duration::from_secs(15);
/// How often to look for a Discord client that is not running.
const RETRY_GAP: Duration = Duration::from_secs(15);

/// The activity shown: whether the player is in the game or has the overlay (a pause) open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Activity {
    Playing,
    InMenu,
}

impl Activity {
    fn state(self) -> &'static str {
        match self {
            Activity::Playing => "Playing",
            Activity::InMenu => "In the settings menu",
        }
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn write_frame(w: &mut impl Write, op: u32, body: &str) -> std::io::Result<()> {
    let mut f = Vec::with_capacity(8 + body.len());
    f.extend_from_slice(&op.to_le_bytes());
    f.extend_from_slice(&(body.len() as u32).to_le_bytes());
    f.extend_from_slice(body.as_bytes());
    w.write_all(&f)?;
    w.flush()
}

fn read_frame(r: &mut impl Read) -> std::io::Result<(u32, String)> {
    let mut head = [0u8; 8];
    r.read_exact(&mut head)?;
    let op = u32::from_le_bytes(head[..4].try_into().unwrap());
    let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
    if len > 64 * 1024 {
        return Err(std::io::Error::other("oversized frame"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok((op, String::from_utf8_lossy(&body).into_owned()))
}

/// A connected, handshaken client.
struct Client<S: Read + Write> {
    stream: S,
    nonce: u64,
}

impl<S: Read + Write> Client<S> {
    fn handshake(mut stream: S, client_id: &str) -> std::io::Result<Client<S>> {
        write_frame(
            &mut stream,
            OP_HANDSHAKE,
            &format!("{{\"v\":1,\"client_id\":{}}}", json_str(client_id)),
        )?;
        let (_, reply) = read_frame(&mut stream)?;
        if !reply.contains("\"READY\"") {
            return Err(std::io::Error::other(format!("Discord refused: {reply}")));
        }
        Ok(Client { stream, nonce: 0 })
    }

    fn set_activity(&mut self, activity: Activity, start: u64) -> std::io::Result<()> {
        self.nonce += 1;
        let body = format!(
            "{{\"cmd\":\"SET_ACTIVITY\",\"args\":{{\"pid\":{},\"activity\":{{\"details\":{},\"state\":{},\"timestamps\":{{\"start\":{start}}}}}}},\"nonce\":\"{}\"}}",
            std::process::id(),
            json_str("Fire Emblem: Path of Radiance"),
            json_str(activity.state()),
            self.nonce
        );
        write_frame(&mut self.stream, OP_FRAME, &body)?;
        read_frame(&mut self.stream).map(|_| ())
    }

    /// Remove the activity (no `activity` argument).
    fn clear(&mut self) -> std::io::Result<()> {
        self.nonce += 1;
        let body = format!(
            "{{\"cmd\":\"SET_ACTIVITY\",\"args\":{{\"pid\":{}}},\"nonce\":\"{}\"}}",
            std::process::id(),
            self.nonce
        );
        write_frame(&mut self.stream, OP_FRAME, &body)?;
        read_frame(&mut self.stream).map(|_| ())
    }
}

/// The folders Discord puts its sockets in, as its own client library looks for them.
#[cfg(unix)]
fn socket_dirs() -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .iter()
        .filter_map(|v| std::env::var_os(v).filter(|d| !d.is_empty()))
        .map(PathBuf::from)
        .collect();
    bases.push(PathBuf::from("/tmp"));
    let mut dirs = Vec::new();
    for b in bases {
        // sandboxed Discord (Flatpak, Snap) keeps its socket in a subfolder
        dirs.push(b.join("app/com.discordapp.Discord"));
        dirs.push(b.join("snap.discord"));
        dirs.push(b);
    }
    dirs
}

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = std::fs::File;

#[cfg(unix)]
fn connect(dirs: &[PathBuf], client_id: &str) -> Option<Client<Stream>> {
    use std::os::unix::net::UnixStream;
    for dir in dirs {
        for n in 0..10 {
            let Ok(s) = UnixStream::connect(dir.join(format!("discord-ipc-{n}"))) else {
                continue;
            };
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
            match Client::handshake(s, client_id) {
                Ok(c) => return Some(c),
                Err(e) => eprintln!("presence: {}: {e}", dir.display()),
            }
        }
    }
    None
}

#[cfg(windows)]
fn connect(_: &[PathBuf], client_id: &str) -> Option<Client<Stream>> {
    for n in 0..10 {
        let pipe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!(r"\\?\pipe\discord-ipc-{n}"));
        if let Ok(p) = pipe {
            match Client::handshake(p, client_id) {
                Ok(c) => return Some(c),
                Err(e) => eprintln!("presence: pipe {n}: {e}"),
            }
        }
    }
    None
}

#[cfg(windows)]
fn socket_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// Watch the setting and keep Discord's display up to date. Does nothing while presence is off.
pub fn spawn(overlay: Arc<Shared>) {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let spawned = std::thread::Builder::new()
        .name("presence".into())
        .spawn(move || run(&overlay, started));
    if let Err(e) = spawned {
        eprintln!("presence: cannot start: {e}");
    }
}

fn run(overlay: &Shared, started: u64) {
    let dirs = socket_dirs();
    let mut client: Option<Client<Stream>> = None;
    let mut last_try: Option<Instant> = None;
    let mut sent: Option<(Activity, Instant)> = None;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let p = overlay.settings.get().presence;
        if !p.enabled || p.client_id.trim().is_empty() {
            // turned off while connected: take the activity down, then leave the socket
            if let Some(mut c) = client.take() {
                let _ = c.clear();
            }
            sent = None;
            last_try = None;
            continue;
        }
        if client.is_none() {
            if last_try.is_some_and(|t| t.elapsed() < RETRY_GAP) {
                continue;
            }
            last_try = Some(Instant::now());
            client = connect(&dirs, p.client_id.trim());
            sent = None;
            if client.is_none() {
                continue;
            }
        }
        let now = if overlay.is_open() {
            Activity::InMenu
        } else {
            Activity::Playing
        };
        if sent.is_some_and(|(a, t)| a == now || t.elapsed() < MIN_UPDATE_GAP) {
            continue;
        }
        if let Some(c) = &mut client {
            match c.set_activity(now, started) {
                Ok(()) => sent = Some((now, Instant::now())),
                Err(e) => {
                    eprintln!("presence: connection lost: {e}");
                    client = None;
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A stand-in for the Discord client: answers the handshake with READY, acknowledges each command and
    /// returns the frames it received.
    fn fake_discord(
        dir: &std::path::Path,
        commands: usize,
    ) -> std::thread::JoinHandle<Vec<(u32, String)>> {
        let listener = UnixListener::bind(dir.join("discord-ipc-0")).unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut got = vec![read_frame(&mut s).unwrap()];
            write_frame(&mut s, OP_FRAME, "{\"cmd\":\"DISPATCH\",\"evt\":\"READY\"}").unwrap();
            for _ in 0..commands {
                got.push(read_frame(&mut s).unwrap());
                write_frame(&mut s, OP_FRAME, "{\"cmd\":\"SET_ACTIVITY\"}").unwrap();
            }
            got
        })
    }

    #[test]
    fn handshake_activity_and_clear_reach_a_fake_discord_socket() {
        let dir = tempfile::tempdir().unwrap();
        let server = fake_discord(dir.path(), 2);
        let mut c = connect(&[dir.path().to_path_buf()], "12\"34").expect("connects");
        c.set_activity(Activity::InMenu, 1700000000).unwrap();
        c.clear().unwrap();
        let got = server.join().unwrap();
        assert_eq!(
            got[0],
            (0, "{\"v\":1,\"client_id\":\"12\\\"34\"}".to_string())
        );
        assert_eq!(got[1].0, 1);
        let act = &got[1].1;
        assert!(act.contains("\"cmd\":\"SET_ACTIVITY\""));
        assert!(act.contains("\"state\":\"In the settings menu\""));
        assert!(act.contains("\"timestamps\":{\"start\":1700000000}"));
        assert!(act.contains("\"nonce\":\"1\""));
        assert!(!got[2].1.contains("activity\""));
    }

    #[test]
    fn a_refusal_is_not_a_connection() {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("discord-ipc-0")).unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            read_frame(&mut s).unwrap();
            write_frame(
                &mut s,
                2,
                "{\"code\":4000,\"message\":\"Invalid Client ID\"}",
            )
            .unwrap();
        });
        assert!(connect(&[dir.path().to_path_buf()], "x").is_none());
    }

    #[test]
    fn no_discord_means_no_connection() {
        let dir = tempfile::tempdir().unwrap();
        assert!(connect(&[dir.path().to_path_buf()], "x").is_none());
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_str("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }
}
