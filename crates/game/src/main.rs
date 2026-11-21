//! Development build of the recompiled game: the runtime plus the generated C++ library.
//!
//! `fepor-game --game <iso|folder> [--data <dir>]`

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use fepor_core::dol::Dol;
use fepor_core::{open_disc, version};
use fepor_runtime::abi::{self, HostFn};
use fepor_runtime::si::{self, button, PadState};
use fepor_runtime::{boot, machine, Context};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

struct Args {
    game: PathBuf,
    data: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut game = None;
    let mut data = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--game" | "--disc" => game = it.next().map(PathBuf::from),
            "--data" | "--output" => data = it.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        game: game.ok_or("usage: fepor-game --game <iso|folder> [--data <dir>]")?,
        data,
    })
}

/// Memory cards: raw Dolphin-format images, `<data>/memcards/MemoryCard{A,B}.<region>.raw`, or the files named
/// by `FEPOR_MEMCARD_A` / `FEPOR_MEMCARD_B` (`none` leaves the slot empty). New cards are 16 Mbit (251 blocks)
/// and unformatted; the game offers to format them.
fn insert_cards(m: &mut fepor_runtime::Machine, game_id: &str, data: Option<&std::path::Path>) {
    let region = match game_id.as_bytes().get(3) {
        Some(b'E') => "USA",
        Some(b'J') => "JAP",
        _ => "EUR",
    };
    let base = data.map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        home.join("fepor-recomp")
    });
    for (slot, letter) in ["A", "B"].into_iter().enumerate() {
        // slot B stays empty unless asked for
        let var = std::env::var(format!("FEPOR_MEMCARD_{letter}")).ok();
        let path = match (&var, slot) {
            (Some(v), _) if v == "none" => continue,
            (Some(v), _) => PathBuf::from(v),
            (None, 0) => base
                .join("memcards")
                .join(format!("MemoryCardA.{region}.raw")),
            (None, _) => continue,
        };
        match fepor_runtime::card::MemCard::open(&path, 16) {
            Ok(card) => m.hw.insert_card(slot, card),
            Err(e) => eprintln!("memory card {letter}: {e}"),
        }
    }
}

fn run(window: Option<Arc<Window>>) -> Result<(), String> {
    // Keep the exported runtime symbols linked for the generated library.
    std::hint::black_box(abi::exports());

    let args = parse_args()?;
    let mut disc = open_disc(&args.game).map_err(|e| e.to_string())?;
    let bytes = disc.dol().map_err(|e| e.to_string())?;
    version::check(disc.game_id(), &bytes).map_err(|e| e.to_string())?;
    let dol = Dol::parse(bytes).map_err(|e| e.to_string())?;

    let m = machine();
    if std::env::var_os("FEPOR_NO_VIDEO").is_none() {
        match fepor_video::Renderer::new(window) {
            Ok(r) => m.hw.set_sink(Box::new(r)),
            Err(e) => eprintln!("video disabled: {e}"),
        }
    }
    if let Some(sink) = fepor_audio::open() {
        m.hw.set_audio_sink(sink);
    }
    insert_cards(m, disc.game_id(), args.data.as_deref());
    let entry = boot::setup(m, &mut *disc, &dol)?;
    boot::attach_disc(m, disc);
    abi::start_watchdog();
    let mut ctx = Context::new(m.ram_ptr());
    fepor_runtime::sched::init(&mut *ctx);
    let start: HostFn = abi::find_function(entry)
        .ok_or_else(|| format!("entry point {entry:#010x} has no generated function"))?;
    fepor_runtime::hle::run(start, &mut ctx);
    Err("the game returned from its entry point".into())
}

/// Run the guest to completion on this thread (a fresh thread with a large stack) and exit the process.
fn spawn_guest(window: Option<Arc<Window>>) {
    // Guest calls nest on the host stack; give the guest thread plenty of room.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let code = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(window)))
            {
                Ok(Ok(())) => 0,
                Ok(Err(e)) => {
                    eprintln!("error: {e}");
                    1
                }
                Err(_) => 70,
            };
            std::process::exit(code);
        })
        .expect("spawn guest thread");
}

/// Keyboard to GameCube controller: arrows = stick, IJKL = C stick, WASD = D-pad, Z/X/C/V = A/B/X/Y,
/// Enter = Start, Q/E = L/R, Space = Z.
fn pad_from_keys(keys: &HashSet<KeyCode>) -> PadState {
    let down = |k| keys.contains(&k);
    let axis = |neg, pos| {
        128u8
            .wrapping_add(if down(pos) { 100 } else { 0 })
            .wrapping_sub(if down(neg) { 100 } else { 0 })
    };
    let mut b = 0;
    for (key, bit) in [
        (KeyCode::KeyZ, button::A),
        (KeyCode::KeyX, button::B),
        (KeyCode::KeyC, button::X),
        (KeyCode::KeyV, button::Y),
        (KeyCode::Enter, button::START),
        (KeyCode::KeyQ, button::L),
        (KeyCode::KeyE, button::R),
        (KeyCode::Space, button::Z),
        (KeyCode::KeyW, button::UP),
        (KeyCode::KeyS, button::DOWN),
        (KeyCode::KeyA, button::LEFT),
        (KeyCode::KeyD, button::RIGHT),
    ] {
        if down(key) {
            b |= bit;
        }
    }
    PadState {
        buttons: b,
        stick: (
            axis(KeyCode::ArrowLeft, KeyCode::ArrowRight),
            axis(KeyCode::ArrowDown, KeyCode::ArrowUp),
        ),
        c_stick: (
            axis(KeyCode::KeyJ, KeyCode::KeyL),
            axis(KeyCode::KeyK, KeyCode::KeyI),
        ),
        triggers: (
            if down(KeyCode::KeyQ) { 255 } else { 0 },
            if down(KeyCode::KeyE) { 255 } else { 0 },
        ),
    }
}

#[derive(Default)]
struct App {
    started: bool,
    keys: HashSet<KeyCode>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        let attrs = Window::default_attributes()
            .with_title("Fire Emblem: Path of Radiance (fepor-recomp)")
            .with_inner_size(LogicalSize::new(640.0, 480.0));
        match el.create_window(attrs) {
            Ok(w) => spawn_guest(Some(Arc::new(w))),
            Err(e) => {
                eprintln!("error: cannot create a window: {e}");
                std::process::exit(1);
            }
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => std::process::exit(0),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        repeat: false,
                        ..
                    },
                ..
            } => {
                if state == ElementState::Pressed {
                    self.keys.insert(code);
                } else {
                    self.keys.remove(&code);
                }
                si::set_pad(pad_from_keys(&self.keys));
            }
            _ => {}
        }
    }
}

fn main() {
    // FEPOR_HEADLESS=1 renders without a window (frame dumps); FEPOR_NO_VIDEO=1 does not render at all.
    if std::env::var_os("FEPOR_HEADLESS").is_some() || std::env::var_os("FEPOR_NO_VIDEO").is_some()
    {
        spawn_guest(None);
        loop {
            std::thread::park();
        }
    }
    let el = EventLoop::new().unwrap_or_else(|e| {
        eprintln!("error: no display ({e}); set FEPOR_HEADLESS=1 to run without a window");
        std::process::exit(1)
    });
    el.set_control_flow(ControlFlow::Wait);
    if let Err(e) = el.run_app(&mut App::default()) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
