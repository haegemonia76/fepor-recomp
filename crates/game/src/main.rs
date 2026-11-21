//! Development build of the recompiled game: the runtime plus the generated C++ library.
//!
//! `fepor-game --game <iso|folder> [--data <dir>]`

use std::path::PathBuf;

use fepor_core::dol::Dol;
use fepor_core::{open_disc, version};
use fepor_runtime::abi::{self, HostFn};
use fepor_runtime::{boot, machine, Context};

struct Args {
    game: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut game = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--game" | "--disc" => game = it.next().map(PathBuf::from),
            "--data" | "--output" => {
                it.next(); // accepted for RecompLauncher compatibility; the runtime writes nothing yet
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        game: game.ok_or("usage: fepor-game --game <iso|folder> [--data <dir>]")?,
    })
}

fn run() -> Result<(), String> {
    // Keep the exported runtime symbols linked for the generated library.
    std::hint::black_box(abi::exports());

    let args = parse_args()?;
    let mut disc = open_disc(&args.game).map_err(|e| e.to_string())?;
    let bytes = disc.dol().map_err(|e| e.to_string())?;
    version::check(disc.game_id(), &bytes).map_err(|e| e.to_string())?;
    let dol = Dol::parse(bytes).map_err(|e| e.to_string())?;

    let m = machine();
    if std::env::var_os("FEPOR_NO_VIDEO").is_none() {
        match fepor_video::Renderer::new() {
            Ok(r) => m.hw.set_sink(Box::new(r)),
            Err(e) => eprintln!("video disabled: {e}"),
        }
    }
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

fn main() {
    // Guest calls nest on the host stack; give the guest thread plenty of room.
    let guest = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(run)
        .expect("spawn guest thread");
    match guest.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        Err(_) => std::process::exit(70),
    }
}
