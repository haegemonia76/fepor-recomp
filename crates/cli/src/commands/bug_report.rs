//! `fepor-recomp bug-report`: the bundle of ticket #47 (`fepor_core::bugreport`), for the data folder.

use std::path::PathBuf;

use fepor_core::bugreport;

use crate::session::{Fail, Options};

pub fn run(opts: &Options, out: Option<PathBuf>) -> Result<(), Fail> {
    let data = opts.output()?;
    let out = match out {
        Some(o) => o,
        None => bugreport::default_output(data).map_err(Fail::Other)?,
    };
    let zip = bugreport::create(data, &out, &bugreport::home_dirs()).map_err(Fail::Other)?;
    println!("bug report: {}", zip.display());
    println!("It holds the log, settings.toml, input.toml, version info and a screenshot if there is one: no saves, no game data, no home folder in paths. Attach it to the report.");
    Ok(())
}
