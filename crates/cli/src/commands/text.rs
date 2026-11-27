//! `text-extract` / `text-build`: the game's message files as editable text and back as a mod (ADR 0025).

use std::fs;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use fepor_core::text::MessageFile;
use fepor_core::{Disc, FileEntry};

use crate::session::{Fail, Options, Session};

const DIR: &str = "Mess";
const EDIT_EXT: &str = ".m.txt";

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Lang {
    En,
    De,
    Fr,
    It,
    Es,
    All,
}

impl Lang {
    /// Language of a message file: `d_`, `f_`, `i_`, `s_` prefixes are German, French, Italian, Spanish; no prefix is English.
    fn of(file: &str) -> Lang {
        match file.as_bytes() {
            [b'd', b'_', ..] => Lang::De,
            [b'f', b'_', ..] => Lang::Fr,
            [b'i', b'_', ..] => Lang::It,
            [b's', b'_', ..] => Lang::Es,
            _ => Lang::En,
        }
    }
}

fn io<T>(r: std::io::Result<T>, p: &Path) -> Result<T, Fail> {
    r.map_err(|e| Fail::Other(format!("{}: {e}", p.display())))
}

fn message_files(disc: &dyn Disc) -> Vec<FileEntry> {
    disc.files()
        .iter()
        .filter(|f| {
            f.path
                .strip_prefix("Mess/")
                .is_some_and(|n| n.ends_with(".m") && !n.contains('/'))
        })
        .cloned()
        .collect()
}

fn read(disc: &mut dyn Disc, f: &FileEntry) -> Result<Vec<u8>, Fail> {
    let mut data = Vec::with_capacity(f.size as usize);
    disc.read_file(f, &mut data)
        .map_err(|e| Fail::Disc(e.to_string()))?;
    Ok(data)
}

fn parse(name: &str, data: &[u8]) -> Result<MessageFile, Fail> {
    MessageFile::parse(data).map_err(|e| Fail::Other(format!("{name}: {e}")))
}

pub fn extract(opts: &Options, dir: &Path, lang: Lang) -> Result<(), Fail> {
    let mut session = Session::open(opts)?;
    let out = dir.join(DIR);
    io(fs::create_dir_all(&out), &out)?;
    let (mut files, mut messages) = (0, 0);
    for f in message_files(&*session.disc) {
        let name = &f.path[DIR.len() + 1..];
        if lang != Lang::All && Lang::of(name) != lang {
            continue;
        }
        let parsed = parse(&f.path, &read(&mut *session.disc, &f)?)?;
        let target = out.join(format!("{name}.txt"));
        io(fs::write(&target, parsed.to_edit(&f.path)), &target)?;
        files += 1;
        messages += parsed.messages.len();
    }
    eprintln!(
        "extracted {messages} messages of {files} files to {}",
        out.display()
    );
    Ok(())
}

pub fn build(opts: &Options, dir: &Path, modroot: &Path, all: bool) -> Result<(), Fail> {
    let src = dir.join(DIR);
    let mut edits: Vec<PathBuf> = io(fs::read_dir(&src), &src)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(EDIT_EXT))
        })
        .collect();
    edits.sort();
    if edits.is_empty() {
        return Err(Fail::Usage(format!(
            "no *{EDIT_EXT} files in {}",
            src.display()
        )));
    }
    let mut session = Session::open(opts)?;
    let on_disc = message_files(&*session.disc);
    let out = modroot.join("files").join(DIR);
    let (mut written, mut unchanged) = (0, 0);
    for p in edits {
        let edit_name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let name = &edit_name[..edit_name.len() - ".txt".len()];
        let path = format!("{DIR}/{name}");
        let Some(entry) = on_disc.iter().find(|f| f.path == path) else {
            return Err(Fail::Usage(format!(
                "{path} is not a message file of this disc"
            )));
        };
        let text = io(fs::read_to_string(&p), &p)?;
        let edited = MessageFile::from_edit(&text)
            .map_err(|e| Fail::Usage(format!("{}: {e}", p.display())))?;
        let original = read(&mut *session.disc, entry)?;
        edited
            .check_against(&parse(&path, &original)?)
            .map_err(|e| Fail::Usage(format!("{}: {e}", p.display())))?;
        let built = edited
            .build()
            .map_err(|e| Fail::Usage(format!("{}: {e}", p.display())))?;
        if built == original && !all {
            unchanged += 1;
            continue;
        }
        io(fs::create_dir_all(&out), &out)?;
        let target = out.join(name);
        io(fs::write(&target, &built), &target)?;
        written += 1;
    }
    eprintln!(
        "wrote {written} files to {} ({unchanged} unchanged, skipped)",
        out.display()
    );
    Ok(())
}
