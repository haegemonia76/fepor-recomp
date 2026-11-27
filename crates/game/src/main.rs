//! Development build of the recompiled game: the runtime plus the generated C++ library.
//!
//! `fepor-game [--data <dir>] [--game <iso|folder>]`: without `--game` the data `prepare` copied to `<data>/disc` is used.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fepor_core::dol::Dol;
use fepor_core::settings::SettingsHandle;
use fepor_core::{open_disc, version};
use fepor_input::Controls;
use fepor_runtime::abi::{self, HostFn};
use fepor_runtime::{boot, machine, Context};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::PhysicalKey;
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

/// What a bug report says about the game: ID, version, region, and the kind and size of the copy (never
/// its path: the folder is only recorded so that the report can remove it).
fn note_game(data: &std::path::Path, game: &std::path::Path, v: &version::Version) {
    let region = match v.game_id.as_bytes().get(3) {
        Some(b'P') => "Europe",
        Some(b'E') => "USA",
        Some(b'J') => "Japan",
        _ => "unknown",
    };
    let copy = match std::fs::metadata(game) {
        Ok(m) if m.is_file() => format!(
            "disc image .{}, {} bytes",
            game.extension().and_then(|e| e.to_str()).unwrap_or("?"),
            m.len()
        ),
        _ => "extracted folder".into(),
    };
    fepor_core::bugreport::update_session(data, |s| {
        s.game_id = v.game_id.into();
        s.disc_dir = if game.is_dir() { game } else { game.parent().unwrap_or(game) }
            .to_string_lossy()
            .into_owned();
    });
    fepor_core::bugreport::note(
        "game",
        format!("game ID: {}\nversion: {}\nregion: {region}\ncopy: {copy}", v.game_id, v.name),
    );
}

/// What the game process records for `fepor-recomp bug-report` at start: build, engine, the `FEPOR_*` and
/// `RECOMPLAUNCHER_*` variables and the command line (the report anonymises them).
fn note_start(data: &std::path::Path) {
    fepor_core::bugreport::update_session(data, |s| {
        s.build = fepor_core::bugreport::build_string();
        s.engine = if cfg!(feature = "jit") { "jit" } else { "cpp" }.into();
        s.trace = std::env::var("FEPOR_TRACE").unwrap_or_default();
    });
    let mut vars: Vec<String> = std::env::vars()
        .filter(|(k, _)| k.starts_with("FEPOR_") || k.starts_with("RECOMPLAUNCHER_"))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    vars.sort();
    fepor_core::bugreport::note(
        "environment",
        if vars.is_empty() { "no FEPOR_* or RECOMPLAUNCHER_* variables set".into() } else { vars.join("\n") },
    );
    let args: Vec<String> = std::env::args().collect();
    fepor_core::bugreport::note("arguments", args.join(" "));
}

/// The state the Help page's bug report adds: the guest machine and the compiler.
fn report_sections(started: std::time::Instant) -> Vec<(String, String)> {
    let mut v = vec![
        (
            "session".to_string(),
            format!("running for {} s (playtime this session)", started.elapsed().as_secs()),
        ),
        ("runtime".to_string(), fepor_runtime::report::state()),
    ];
    #[cfg(feature = "jit")]
    if let Some(j) = fepor_jit::Jit::global() {
        v.push(("jit".to_string(), j.summary()));
    }
    v
}

/// `.gci` saves dropped in `imports` are put on the card image at `card` (and renamed `.imported`). The card
/// must already be formatted: format it in the game first.
fn import_saves(card: &std::path::Path, imports: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(imports) else {
        return;
    };
    let files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "gci"))
        .collect();
    if files.is_empty() {
        return;
    }
    let Ok(mut image) = std::fs::read(card) else {
        eprintln!(
            "saves in {} wait for the memory card to be created and formatted",
            imports.display()
        );
        return;
    };
    let mut changed = false;
    for f in files {
        let done = std::fs::read(&f)
            .map_err(|e| e.to_string())
            .and_then(|gci| fepor_core::memcard::import_gci(&mut image, &gci));
        match done {
            Ok(()) => {
                eprintln!("imported {}", f.display());
                changed = true;
                let _ = std::fs::rename(&f, f.with_extension("gci.imported"));
            }
            Err(e) => {
                // set aside so that it is not tried at every start; the overlay lists it
                eprintln!("cannot import {}: {e}", f.display());
                let _ = std::fs::rename(&f, f.with_extension("gci.failed"));
            }
        }
    }
    if changed {
        if let Err(e) = std::fs::write(card, &image) {
            eprintln!("cannot write {}: {e}", card.display());
        }
    }
}

/// Seconds between backups of a card the game modified.
const BACKUP_EVERY: std::time::Duration = std::time::Duration::from_secs(300);

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn backup_card(card: &std::path::Path, dir: &std::path::Path, overlay: &fepor_overlay::Shared) {
    let keep = overlay.settings.get().game.card_backups as usize;
    match fepor_core::memcard::backup(card, dir, keep, unix_now()) {
        Ok(Some(p)) => eprintln!("memory card backup: {}", p.display()),
        Ok(None) => {}
        Err(e) => eprintln!("memory card backup of {}: {e}", card.display()),
    }
}

/// Apply the restore the overlay queued, back the card up before the run, and keep backing it up while the
/// game modifies it (a copy is made only when the image differs from the newest one).
fn protect_card(card: &std::path::Path, base: &std::path::Path, overlay: &Arc<fepor_overlay::Shared>) {
    let dir = fepor_core::memcard::backups_dir(base);
    if let Some(from) = fepor_core::memcard::take_restore(&dir) {
        // the card being replaced is itself kept as a backup first, and read before rotation can drop `from`
        let image = std::fs::read(&from);
        backup_card(card, &dir, overlay);
        match image.and_then(|_| fepor_core::memcard::restore(card, &from)) {
            Ok(()) => eprintln!("memory card restored from {}", from.display()),
            Err(e) => eprintln!("cannot restore {}: {e}", from.display()),
        }
    }
    backup_card(card, &dir, overlay);
    let (card, overlay) = (card.to_path_buf(), overlay.clone());
    let timer = move || loop {
        std::thread::sleep(BACKUP_EVERY);
        backup_card(&card, &dir, &overlay);
    };
    if let Err(e) = std::thread::Builder::new().name("card-backup".into()).spawn(timer) {
        eprintln!("memory card backups while playing are off: {e}");
    }
}

/// Memory cards: raw Dolphin-format images, `<data>/memcards/MemoryCard{A,B}.<region>.raw`, or the files named
/// by `FEPOR_MEMCARD_A` / `FEPOR_MEMCARD_B` (`none` leaves the slot empty). New cards are 16 Mbit (251 blocks)
/// and unformatted; the game offers to format them.
fn insert_cards(
    m: &mut fepor_runtime::Machine,
    game_id: &str,
    data: Option<&std::path::Path>,
    overlay: &Arc<fepor_overlay::Shared>,
) {
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
        if slot == 0 {
            protect_card(&path, &base, overlay);
            import_saves(&path, &base.join("imports"));
            *overlay.card_path.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.clone());
        }
        match fepor_runtime::card::MemCard::open(&path, 16) {
            Ok(card) => m.hw.insert_card(slot, card),
            Err(e) => eprintln!("memory card {letter}: {e}"),
        }
    }
}

/// Compile guest code in process: analyse the DOL (about a second), then compile functions on first call.
#[cfg(feature = "jit")]
fn install_jit(game_id: &str, dol: &Dol) {
    let cfg = version::lookup(game_id)
        .map(|v| v.analysis_defaults())
        .unwrap_or_default();
    let analysis = fepor_core::analysis::analyze(dol, &cfg);
    let trace_calls = std::env::var("FEPOR_TRACE")
        .is_ok_and(|v| v.split(',').any(|x| x == "calls" || x == "all"));
    let symbols = fepor_runtime::hle::symbols();
    let jit = fepor_jit::Jit::install(dol.clone(), &analysis, &cfg.hle, &symbols, trace_calls);
    abi::set_resolver(|target| fepor_jit::Jit::global()?.entry(target));
    // The rest of the game's code is compiled in the background, ahead of its first call.
    // FEPOR_JIT_WORKERS=0 turns it off (functions then compile on first call).
    let workers = std::env::var("FEPOR_JIT_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| {
            let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
            (cores / 4).clamp(1, 4)
        });
    jit.precompile(symbols, workers);
}

/// Serve the files of the enabled mods instead of the disc's (ADR 0034); the rest of the disc is untouched.
fn replace_files(
    disc: Box<dyn fepor_core::Disc>,
    mods: &fepor_core::mods::ModList,
    game_id: &str,
) -> Result<Box<dyn fepor_core::Disc>, String> {
    for c in mods.conflicts(Some(game_id)) {
        eprintln!("mods: {c}");
    }
    let files = mods.replacements(game_id);
    if files.is_empty() {
        return Ok(disc);
    }
    let modded = fepor_core::disc::ModdedDisc::new(disc, &files).map_err(|e| e.to_string())?;
    for path in &modded.unmatched {
        eprintln!("mods: {path} is not a file of this disc: ignored");
    }
    eprintln!("mods: {} disc file(s) replaced", modded.replaced());
    Ok(Box::new(modded))
}

/// Register and enable the memory patches of the enabled mods, highest priority first. A mod whose bytes
/// another enabled mod already patches stays off (the engine refuses it). Patches over the DOL's code change
/// nothing (ADR 0025), so they are reported.
fn apply_patch_mods(
    m: &mut fepor_runtime::Machine,
    mods: &fepor_core::mods::ModList,
    game_id: &str,
    dol: &Dol,
) {
    use fepor_runtime::patch::{Patch, PatchSet};
    for md in mods.patch_mods(game_id) {
        let patches: Vec<Patch> = md
            .manifest
            .patches
            .iter()
            .map(|p| Patch::new(p.address, p.bytes.clone()))
            .collect();
        for p in &md.manifest.patches {
            let r = p.range();
            let over_code = |s: &fepor_core::dol::Section| {
                let start = (s.addr & 0x3FFF_FFFF) as u64;
                r.start < start + s.size as u64 && start < r.end
            };
            if dol.text_sections().any(over_code) {
                eprintln!("mods: {}: patch at {:#010x} is over the game's code and changes nothing", md.id, p.address);
            }
        }
        let set = PatchSet {
            source: format!("mod:{}", md.id),
            game_id: game_id.to_string(),
            patches,
            continuous: md.manifest.continuous,
        };
        let done = m
            .patches
            .register(set)
            .and_then(|id| m.patches.enable(id, &mut m.ram));
        match done {
            Ok(()) => eprintln!("mods: {}: {} patch(es) applied", md.id, md.manifest.patches.len()),
            Err(e) => eprintln!("mods: {}: patches not applied: {e}", md.id),
        }
    }
}

fn run(window: Option<Arc<Window>>, overlay: Arc<fepor_overlay::Shared>) -> Result<(), String> {
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
    let game_id = disc.game_id().to_string();
    let mods = fepor_core::mods::ModList::load(&data);
    disc = replace_files(disc, &mods, &game_id)?;
    let bytes = disc.dol().map_err(|e| e.to_string())?;
    let version = version::check(disc.game_id(), &bytes).map_err(|e| e.to_string())?;
    note_game(&data, &game, version);
    let dol = Dol::parse(bytes).map_err(|e| e.to_string())?;
    #[cfg(feature = "jit")]
    install_jit(disc.game_id(), &dol);

    let m = machine();
    if std::env::var_os("FEPOR_NO_VIDEO").is_none() {
        let ui = fepor_overlay::Overlay::new(overlay.clone());
        match fepor_video::Renderer::new(window, Some(ui), overlay.settings.clone()) {
            Ok(r) => m.hw.set_sink(Box::new(r)),
            Err(e) => eprintln!("video disabled: {e}"),
        }
    }
    if let Some(sink) = fepor_audio::open(overlay.settings.clone(), overlay.audio_devices.clone()) {
        m.hw.set_audio_sink(sink);
    }
    m.patches.set_game(disc.game_id()).map_err(|e| e.to_string())?;
    m.patches.attach_watcher(overlay.memory.clone());
    // physical ranges of the DOL's code: recompiled, so cheats writing there would do nothing
    let code = dol
        .text_sections()
        .map(|s| (s.addr & 0x01FF_FFFF)..(s.addr & 0x01FF_FFFF) + s.size)
        .collect();
    overlay.cheats.set_game(disc.game_id(), code);
    m.cheats.attach(overlay.cheats.clone());
    insert_cards(m, disc.game_id(), Some(&data), &overlay);
    m.hw.set_language(overlay.settings.get().game.language.sram_code());
    let entry = boot::setup(m, &mut *disc, &dol)?;
    boot::attach_disc(m, disc);
    apply_patch_mods(m, &mods, &game_id, &dol);
    abi::start_watchdog();
    let mut ctx = Context::new(m.ram_ptr());
    fepor_runtime::sched::init(&mut *ctx);
    let start: HostFn = abi::find_function(entry)
        .ok_or_else(|| format!("entry point {entry:#010x} has no generated function"))?;
    fepor_runtime::hle::run(start, &mut ctx);
    Err("the game returned from its entry point".into())
}

/// Run the guest to completion on this thread (a fresh thread with a large stack) and exit the process.
fn spawn_guest(window: Option<Arc<Window>>, overlay: Arc<fepor_overlay::Shared>) {
    // Guest calls nest on the host stack; give the guest thread plenty of room.
    std::thread::Builder::new()
        .name("guest".into())
        .stack_size(256 << 20)
        .spawn(move || {
            let guest = std::panic::AssertUnwindSafe(|| run(window, overlay));
            let code = match std::panic::catch_unwind(guest) {
                Ok(Ok(())) => 0,
                Ok(Err(e)) => {
                    eprintln!("error: {e}");
                    1
                }
                Err(_) => 70,
            };
            fepor_core::applog::exit(code);
        })
        .expect("spawn guest thread");
}

type SharedControls = Arc<Mutex<Controls>>;

struct App {
    started: bool,
    window: Option<Arc<Window>>,
    controls: SharedControls,
    overlay: Arc<fepor_overlay::Shared>,
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
                spawn_guest(Some(w), self.overlay.clone());
                let (controls, overlay, proxy) = (
                    self.controls.clone(),
                    self.overlay.clone(),
                    self.proxy.clone(),
                );
                fepor_input::gamepad::spawn(move |source, value| {
                    if overlay.gamepad_input(source, value) {
                        return;
                    }
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
                fepor_core::applog::exit(1);
            }
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _: ()) {
        self.refresh_title();
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
        if self.overlay.window_event(&event, scale) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => fepor_core::applog::exit(0),
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
                    c.input(true, &format!("{code:?}"), if pressed { 1.0 } else { 0.0 });
                }
                self.refresh_title();
            }
            _ => {}
        }
    }
}

fn main() {
    // FEPOR_HEADLESS=1 renders without a window (frame dumps); FEPOR_NO_VIDEO=1 does not render at all.
    let data = data_dir();
    fepor_core::applog::install(&data);
    fepor_core::bugreport::init(&data);
    note_start(&data);
    let controls = Arc::new(Mutex::new(Controls::new(data.join("input.toml"))));
    let overlay = fepor_overlay::Shared::new(SettingsHandle::load(&data), controls.clone(), data);
    let started = std::time::Instant::now();
    overlay.set_report_hook(Box::new(move || report_sections(started)));
    if std::env::var_os("FEPOR_HEADLESS").is_some() || std::env::var_os("FEPOR_NO_VIDEO").is_some()
    {
        spawn_guest(None, overlay);
        loop {
            std::thread::park();
        }
    }
    let el = EventLoop::<()>::with_user_event()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("error: no display ({e}); set FEPOR_HEADLESS=1 to run without a window");
            fepor_core::applog::exit(1)
        });
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        started: false,
        window: None,
        controls,
        overlay,
        proxy: el.create_proxy(),
    };
    if let Err(e) = el.run_app(&mut app) {
        eprintln!("error: {e}");
        fepor_core::applog::exit(1);
    }
}
