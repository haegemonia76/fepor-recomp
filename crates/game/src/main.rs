//! Development build of the recompiled game: the runtime plus the generated C++ library.
//!
//! `fepor-game [--data <dir>] [--game <iso|folder>]`: without `--game` the data `prepare` copied to `<data>/disc` is used.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fepor_core::dol::Dol;
use fepor_core::{open_disc, version};
use fepor_input::Controls;
use fepor_runtime::abi::{self, HostFn};
use fepor_runtime::{boot, machine, Context};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

struct Args {
    game: Option<PathBuf>,
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
    Ok(Args { game, data })
}

/// Where the prepared game, saves and settings live: see `fepor_core::paths::default_data_dir`.
fn default_data_dir() -> PathBuf {
    fepor_core::paths::default_data_dir()
}

/// The data directory from the command line, or the default.
fn data_dir() -> PathBuf {
    parse_args()
        .ok()
        .and_then(|a| a.data)
        .unwrap_or_else(default_data_dir)
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
    let base = data.map(PathBuf::from).unwrap_or_else(default_data_dir);
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
    let data = args.data.clone().unwrap_or_else(default_data_dir);
    // The disc image when given, else the game data `prepare` copied next to the build: no disc needed.
    let game = args.game.clone().or_else(|| fepor_core::paths::prepared_disc(&data)).ok_or_else(|| {
        format!(
            "no game data in {}: run the recompilation first (fepor-recomp-gui or `fepor-recomp prepare`), or pass --game <iso|folder>",
            data.display()
        )
    })?;
    let mut disc = open_disc(&game).map_err(|e| e.to_string())?;
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
    insert_cards(m, disc.game_id(), Some(&data));
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

type Shared = Arc<Mutex<Controls>>;

struct App {
    started: bool,
    window: Option<Arc<Window>>,
    controls: Shared,
    proxy: EventLoopProxy<()>,
}

impl App {
    /// Show the controls' pending message in the window title.
    fn refresh_title(&mut self) {
        let msg = self.controls.lock().ok().and_then(|mut c| c.message.take());
        if let (Some(w), Some(m)) = (&self.window, msg) {
            w.set_title(&format!(
                "Fire Emblem: Path of Radiance (fepor-recomp) - {m}"
            ));
        }
    }
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        let attrs = Window::default_attributes()
            .with_title("Fire Emblem: Path of Radiance (fepor-recomp)")
            .with_inner_size(LogicalSize::new(640.0, 480.0));
        match el.create_window(attrs) {
            Ok(w) => {
                let w = Arc::new(w);
                self.window = Some(w.clone());
                spawn_guest(Some(w));
                let (controls, proxy) = (self.controls.clone(), self.proxy.clone());
                fepor_input::gamepad::spawn(move |source, value| {
                    if let Ok(mut c) = controls.lock() {
                        c.input(false, source, value);
                        if c.message.is_some() {
                            let _ = proxy.send_event(());
                        }
                    }
                });
            }
            Err(e) => {
                eprintln!("error: cannot create a window: {e}");
                std::process::exit(1);
            }
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _: ()) {
        self.refresh_title();
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => std::process::exit(0),
            WindowEvent::Focused(false) => {
                if let Ok(mut c) = self.controls.lock() {
                    c.release_keyboard();
                }
            }
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
                let pressed = state == ElementState::Pressed;
                if let Ok(mut c) = self.controls.lock() {
                    match (code, pressed) {
                        // hotkeys: F2 remap keyboard, F3 remap gamepad, F4 button glyph set
                        (KeyCode::F2, true) => c.toggle_remap(true),
                        (KeyCode::F3, true) => c.toggle_remap(false),
                        (KeyCode::F4, true) => c.cycle_glyphs(),
                        _ => c.input(true, &format!("{code:?}"), if pressed { 1.0 } else { 0.0 }),
                    }
                }
                self.refresh_title();
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
    let el = EventLoop::<()>::with_user_event()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("error: no display ({e}); set FEPOR_HEADLESS=1 to run without a window");
            std::process::exit(1)
        });
    el.set_control_flow(ControlFlow::Wait);
    let controls = Arc::new(Mutex::new(Controls::new(data_dir().join("input.toml"))));
    let mut app = App {
        started: false,
        window: None,
        controls,
        proxy: el.create_proxy(),
    };
    if let Err(e) = el.run_app(&mut app) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
