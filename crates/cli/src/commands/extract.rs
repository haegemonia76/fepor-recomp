use fepor_core::extract::extract;

use crate::session::{Fail, Options, Session};

pub fn run(opts: &Options) -> Result<(), Fail> {
    let out = opts.output()?;
    let mut session = Session::open(opts)?;
    let report = extract(&mut *session.disc, out, |_| {}).map_err(|e| Fail::Other(e.to_string()))?;
    eprintln!("extracted {} system files and {} files ({} bytes) to {}", report.sys_files, report.files, report.bytes, out.display());
    Ok(())
}
