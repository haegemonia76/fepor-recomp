use std::path::PathBuf;

use fepor_core::analysis::analyze;

use crate::session::{Fail, Options, Session};

pub fn run(opts: &Options, report: Option<PathBuf>) -> Result<(), Fail> {
    let session = Session::open(opts)?;
    let analysis = analyze(&session.dol, &session.analysis_cfg);
    eprintln!("{}", analysis.summary());
    let path = match report {
        Some(p) => Some(p),
        None => opts.output().ok().map(|o| o.join("analysis.txt")),
    };
    if let Some(path) = path {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Fail::Other(format!("{}: {e}", dir.display())))?;
        }
        std::fs::write(&path, analysis.report())
            .map_err(|e| Fail::Other(format!("{}: {e}", path.display())))?;
        eprintln!("report written to {}", path.display());
    }
    Ok(())
}
