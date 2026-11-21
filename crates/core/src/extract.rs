use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use crate::disc::{Disc, DiscError};

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error(transparent)]
    Disc(#[from] DiscError),
    #[error("I/O error on {0}: {1}")]
    Io(std::path::PathBuf, std::io::Error),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExtractReport {
    pub sys_files: usize,
    pub files: usize,
    pub bytes: u64,
}

/// Extract `sys/` and `files/` (Dolphin folder layout) into `out`.
/// `on_file` is called with each file's relative path before it is written.
pub fn extract(disc: &mut dyn Disc, out: &Path, mut on_file: impl FnMut(&str)) -> Result<ExtractReport, ExtractError> {
    let io = |p: &Path| {
        let p = p.to_path_buf();
        move |e| ExtractError::Io(p, e)
    };
    let mut report = ExtractReport::default();

    let sys_dir = out.join("sys");
    fs::create_dir_all(&sys_dir).map_err(io(&sys_dir))?;
    for (name, data) in disc.sys_files()? {
        let p = sys_dir.join(name);
        fs::write(&p, &data).map_err(io(&p))?;
        report.sys_files += 1;
        report.bytes += data.len() as u64;
    }

    let files_dir = out.join("files");
    let entries = disc.files().to_vec();
    for entry in &entries {
        on_file(&entry.path);
        let p = files_dir.join(&entry.path);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(io(parent))?;
        }
        let mut w = BufWriter::new(File::create(&p).map_err(io(&p))?);
        disc.read_file(entry, &mut w)?;
        std::io::Write::flush(&mut w).map_err(io(&p))?;
        report.files += 1;
        report.bytes += entry.size;
    }
    Ok(report)
}
