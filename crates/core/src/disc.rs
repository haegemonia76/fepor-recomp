//! Abstract disc reader: ISO/GCM, RVZ and GCZ images (see `image`) and the extracted folder (`sys/` + `files/`, Dolphin layout).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::dol;
use crate::image::{self, Image};

const GCM_MAGIC: u32 = 0xC233_9F3D;
const BOOT_BIN_SIZE: usize = 0x440;
const BI2_OFFSET: u64 = 0x440;
const BI2_SIZE: usize = 0x2000;
const APPLOADER_OFFSET: u64 = 0x2440;
const APPLOADER_HEADER: usize = 0x20;

#[derive(Debug, thiserror::Error)]
pub enum DiscError {
    #[error("I/O error on {0}: {1}")]
    Io(PathBuf, io::Error),
    #[error("not a GameCube disc image: {0}")]
    BadImage(String),
    #[error("unsafe file name in FST: {0:?}")]
    BadName(String),
}

fn io_err(p: &Path) -> impl FnOnce(io::Error) -> DiscError + '_ {
    move |e| DiscError::Io(p.to_path_buf(), e)
}

/// A file in the disc file system. `path` is relative, `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub offset: u64,
    pub size: u64,
}

pub trait Disc {
    /// 6-character game ID (e.g. `GFEP01`).
    fn game_id(&self) -> &str;
    /// System files in Dolphin's `sys/` layout: `boot.bin`, `bi2.bin`, `apploader.img`, `fst.bin`, `main.dol`.
    fn sys_files(&mut self) -> Result<Vec<(&'static str, Vec<u8>)>, DiscError>;
    fn dol(&mut self) -> Result<Vec<u8>, DiscError>;
    fn files(&self) -> &[FileEntry];
    /// Stream one file's content into `out`.
    fn read_file(&mut self, entry: &FileEntry, out: &mut dyn Write) -> Result<(), DiscError>;
    /// Raw read from the disc image at `offset`; bytes outside the image read as zero.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DiscError>;
    /// Length of the disc image in bytes: nothing of the game lives at or after it.
    fn image_len(&self) -> u64;
}

/// Open an ISO/GCM file or an extracted folder.
pub fn open_disc(path: &Path) -> Result<Box<dyn Disc>, DiscError> {
    if path.is_dir() {
        Ok(Box::new(FolderDisc::open(path)?))
    } else {
        Ok(Box::new(IsoDisc::open(path)?))
    }
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}

fn game_id_of(boot: &[u8]) -> Result<String, DiscError> {
    let id = &boot[..6];
    if !id.iter().all(|c| c.is_ascii_alphanumeric()) {
        return Err(DiscError::BadImage("invalid game ID".into()));
    }
    Ok(String::from_utf8_lossy(id).into_owned())
}

/// Parse an FST blob into file entries (directories are flattened into paths).
pub fn parse_fst(fst: &[u8]) -> Result<Vec<FileEntry>, DiscError> {
    Ok(parse_fst_indexed(fst)?
        .into_iter()
        .map(|(_, e)| e)
        .collect())
}

/// Like [`parse_fst`], with each file's index in the FST table (what `rewrite_fst` patches).
pub fn parse_fst_indexed(fst: &[u8]) -> Result<Vec<(usize, FileEntry)>, DiscError> {
    let bad = |m: &str| DiscError::BadImage(format!("FST: {m}"));
    if fst.len() < 12 || fst[0] != 1 {
        return Err(bad("missing root directory"));
    }
    let count = be32(fst, 8) as usize;
    let table = count
        .checked_mul(12)
        .filter(|&t| t <= fst.len())
        .ok_or_else(|| bad("truncated"))?;
    let names = &fst[table..];
    let name_at = |off: usize| -> Result<String, DiscError> {
        let tail = names
            .get(off..)
            .ok_or_else(|| bad("name offset out of range"))?;
        let end = tail
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| bad("unterminated name"))?;
        let name: String = tail[..end].iter().map(|&b| b as char).collect(); // Latin-1
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
            return Err(DiscError::BadName(name));
        }
        Ok(name)
    };

    let mut out = Vec::new();
    // (directory name, index where the directory ends)
    let mut stack: Vec<(String, usize)> = Vec::new();
    for i in 1..count {
        while stack.last().is_some_and(|&(_, end)| i >= end) {
            stack.pop();
        }
        let e = &fst[i * 12..i * 12 + 12];
        let name = name_at((be32(e, 0) & 0x00FF_FFFF) as usize)?;
        if e[0] != 0 {
            let end = be32(e, 8) as usize;
            if end <= i || end > count {
                return Err(bad("bad directory range"));
            }
            stack.push((name, end));
        } else {
            let mut path = String::new();
            for (d, _) in &stack {
                path.push_str(d);
                path.push('/');
            }
            path.push_str(&name);
            out.push((
                i,
                FileEntry {
                    path,
                    offset: be32(e, 4) as u64,
                    size: be32(e, 8) as u64,
                },
            ));
        }
    }
    Ok(out)
}

pub struct IsoDisc {
    path: PathBuf,
    img: Box<dyn Image>,
    id: String,
    boot: Vec<u8>,
    files: Vec<FileEntry>,
}

impl IsoDisc {
    /// Open a GCM (ISO), GCZ or RVZ image.
    pub fn open(path: &Path) -> Result<Self, DiscError> {
        let mut img = image::open(path)?;
        let mut boot = vec![0u8; BOOT_BIN_SIZE];
        if img.len() < BOOT_BIN_SIZE as u64 {
            return Err(DiscError::BadImage("file too small".into()));
        }
        img.read_at(0, &mut boot).map_err(io_err(path))?;
        if be32(&boot, 0x1C) != GCM_MAGIC {
            return Err(DiscError::BadImage(
                "GCM magic not found: not a GameCube disc image".into(),
            ));
        }
        let id = game_id_of(&boot)?;
        let mut disc = IsoDisc {
            path: path.to_path_buf(),
            img,
            id,
            boot,
            files: Vec::new(),
        };
        let fst = disc.fst()?;
        disc.files = parse_fst(&fst)?;
        Ok(disc)
    }

    /// `len` bytes at `offset`; the image must hold them all.
    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, DiscError> {
        if offset + len as u64 > self.img.len() {
            return Err(DiscError::BadImage("the image ends before its data".into()));
        }
        let mut buf = vec![0u8; len];
        self.img
            .read_at(offset, &mut buf)
            .map_err(io_err(&self.path))?;
        Ok(buf)
    }

    fn fst(&mut self) -> Result<Vec<u8>, DiscError> {
        let (off, len) = (
            be32(&self.boot, 0x424) as u64,
            be32(&self.boot, 0x428) as usize,
        );
        self.read_at(off, len)
    }

    fn dol_bytes(&mut self) -> Result<Vec<u8>, DiscError> {
        let off = be32(&self.boot, 0x420) as u64;
        let info = dol::parse_header(&self.read_at(off, dol::HEADER_SIZE)?)
            .ok_or_else(|| DiscError::BadImage("bad DOL header".into()))?;
        self.read_at(off, info.size)
    }
}

impl Disc for IsoDisc {
    fn game_id(&self) -> &str {
        &self.id
    }

    fn sys_files(&mut self) -> Result<Vec<(&'static str, Vec<u8>)>, DiscError> {
        let hdr = self.read_at(APPLOADER_OFFSET, APPLOADER_HEADER)?;
        let apploader_len =
            APPLOADER_HEADER + be32(&hdr, 0x14) as usize + be32(&hdr, 0x18) as usize;
        Ok(vec![
            ("boot.bin", self.boot.clone()),
            ("bi2.bin", self.read_at(BI2_OFFSET, BI2_SIZE)?),
            (
                "apploader.img",
                self.read_at(APPLOADER_OFFSET, apploader_len)?,
            ),
            ("fst.bin", self.fst()?),
            ("main.dol", self.dol_bytes()?),
        ])
    }

    fn dol(&mut self) -> Result<Vec<u8>, DiscError> {
        self.dol_bytes()
    }

    fn files(&self) -> &[FileEntry] {
        &self.files
    }

    fn read_file(&mut self, entry: &FileEntry, out: &mut dyn Write) -> Result<(), DiscError> {
        if entry.offset + entry.size > self.img.len() {
            return Err(DiscError::BadImage(format!("{} is truncated", entry.path)));
        }
        let mut buf = vec![0u8; 1 << 20];
        let (mut at, end) = (entry.offset, entry.offset + entry.size);
        while at < end {
            let n = buf.len().min((end - at) as usize);
            self.img
                .read_at(at, &mut buf[..n])
                .map_err(io_err(&self.path))?;
            out.write_all(&buf[..n]).map_err(io_err(&self.path))?;
            at += n as u64;
        }
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DiscError> {
        self.img.read_at(offset, buf).map_err(io_err(&self.path))
    }

    fn image_len(&self) -> u64 {
        self.img.len()
    }
}

pub struct FolderDisc {
    root: PathBuf,
    id: String,
    files: Vec<FileEntry>,
    /// (disc offset, length, host file) for every part of the virtual disc image.
    regions: Vec<(u64, u64, PathBuf)>,
}

impl FolderDisc {
    pub fn open(root: &Path) -> Result<Self, DiscError> {
        let boot_path = root.join("sys/boot.bin");
        let mut boot = vec![0u8; BOOT_BIN_SIZE];
        File::open(&boot_path)
            .and_then(|mut f| f.read_exact(&mut boot))
            .map_err(io_err(&boot_path))?;
        let id = game_id_of(&boot)?;
        // Prefer the FST in sys/ (real disc offsets); fall back to scanning the folder.
        let sys = root.join("sys");
        let mut files = match std::fs::read(sys.join("fst.bin"))
            .ok()
            .and_then(|f| parse_fst(&f).ok())
        {
            Some(entries) => entries,
            None => {
                let mut v = Vec::new();
                walk(&root.join("files"), "", &mut v)?;
                v.sort_by(|a, b| a.path.cmp(&b.path));
                v
            }
        };
        files.sort_by(|a, b| a.path.cmp(&b.path));

        // Where each file lives in the virtual disc image, for raw reads (the DVD driver reads by offset).
        let word = |o: usize| u32::from_be_bytes(boot[o..o + 4].try_into().unwrap()) as u64;
        let size_of = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let mut regions = vec![
            (0, size_of(&sys.join("boot.bin")), sys.join("boot.bin")),
            (
                BI2_OFFSET,
                size_of(&sys.join("bi2.bin")),
                sys.join("bi2.bin"),
            ),
            (
                APPLOADER_OFFSET,
                size_of(&sys.join("apploader.img")),
                sys.join("apploader.img"),
            ),
            (
                word(0x424),
                size_of(&sys.join("fst.bin")),
                sys.join("fst.bin"),
            ),
            (
                word(0x420),
                size_of(&sys.join("main.dol")),
                sys.join("main.dol"),
            ),
        ];
        for f in &files {
            regions.push((f.offset, f.size, root.join("files").join(&f.path)));
        }
        regions.sort_by_key(|r| r.0);
        Ok(FolderDisc {
            root: root.to_path_buf(),
            id,
            files,
            regions,
        })
    }

    fn sys(&self, name: &str) -> Result<Vec<u8>, DiscError> {
        let p = self.root.join("sys").join(name);
        std::fs::read(&p).map_err(io_err(&p))
    }
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<FileEntry>) -> Result<(), DiscError> {
    for e in std::fs::read_dir(dir).map_err(io_err(dir))? {
        let e = e.map_err(io_err(dir))?;
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = format!("{prefix}{name}");
        let meta = e.metadata().map_err(io_err(&e.path()))?;
        if meta.is_dir() {
            walk(&e.path(), &format!("{rel}/"), out)?;
        } else {
            out.push(FileEntry {
                path: rel,
                offset: 0,
                size: meta.len(),
            });
        }
    }
    Ok(())
}

impl Disc for FolderDisc {
    fn game_id(&self) -> &str {
        &self.id
    }

    fn sys_files(&mut self) -> Result<Vec<(&'static str, Vec<u8>)>, DiscError> {
        [
            "boot.bin",
            "bi2.bin",
            "apploader.img",
            "fst.bin",
            "main.dol",
        ]
        .into_iter()
        .map(|n| Ok((n, self.sys(n)?)))
        .collect()
    }

    fn dol(&mut self) -> Result<Vec<u8>, DiscError> {
        self.sys("main.dol")
    }

    fn files(&self) -> &[FileEntry] {
        &self.files
    }

    fn read_file(&mut self, entry: &FileEntry, out: &mut dyn Write) -> Result<(), DiscError> {
        let p = self.root.join("files").join(&entry.path);
        let mut f = File::open(&p).map_err(io_err(&p))?;
        io::copy(&mut f, out).map_err(io_err(&p))?;
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DiscError> {
        buf.fill(0);
        let end = offset + buf.len() as u64;
        // regions do not overlap and are sorted by start: skip to the first one that ends after `offset`
        let first = self
            .regions
            .partition_point(|(start, len, _)| start + len <= offset);
        for (start, len, path) in self.regions[first..]
            .iter()
            .take_while(|(start, _, _)| *start < end)
        {
            let (lo, hi) = (offset.max(*start), end.min(start + len));
            if lo >= hi {
                continue;
            }
            let mut f = File::open(path).map_err(io_err(path))?;
            f.seek(SeekFrom::Start(lo - start)).map_err(io_err(path))?;
            let dst = &mut buf[(lo - offset) as usize..(hi - offset) as usize];
            let mut got = 0;
            while got < dst.len() {
                match f.read(&mut dst[got..]).map_err(io_err(path))? {
                    0 => break, // file shorter than the FST says: the rest stays zero
                    n => got += n,
                }
            }
        }
        Ok(())
    }

    fn image_len(&self) -> u64 {
        self.regions
            .iter()
            .map(|(start, len, _)| start + len)
            .max()
            .unwrap_or(0)
    }
}

/// Replacements start on this boundary (the DVD driver reads in 32-byte units).
const REPLACEMENT_ALIGN: u64 = 0x20;

/// Rewrite the FST so that each `(file path, offset, size)` in `moves` points at its new place. Names, the
/// directory structure and the table's length stay as they are. Paths that are not in the FST are an error.
pub fn rewrite_fst(fst: &[u8], moves: &[(String, u64, u64)]) -> Result<Vec<u8>, DiscError> {
    let word = |v: u64| {
        u32::try_from(v).map_err(|_| {
            DiscError::BadImage(
                "a replaced file lies beyond the 4 GiB a GameCube disc can address".into(),
            )
        })
    };
    let mut out = fst.to_vec();
    let entries = parse_fst_indexed(fst)?;
    for (path, offset, size) in moves {
        let (index, _) = entries
            .iter()
            .find(|(_, e)| e.path == *path)
            .ok_or_else(|| DiscError::BadImage(format!("{path} is not on the disc")))?;
        out[index * 12 + 4..index * 12 + 8].copy_from_slice(&word(*offset)?.to_be_bytes());
        out[index * 12 + 8..index * 12 + 12].copy_from_slice(&word(*size)?.to_be_bytes());
    }
    Ok(out)
}

/// A file served from the host instead of the disc: it lives at `start..start + len` of the virtual image,
/// after the end of the real one.
struct Replacement {
    start: u64,
    len: u64,
    host: PathBuf,
}

/// A disc with files replaced by host files (mods, ADR 0034). The guest reads by raw offset and gets its FST
/// from [`Disc::sys_files`], so the replaced files are appended after the end of the image, the FST entries
/// point there, and reads of that range come from the host files. Every other read goes to the wrapped disc.
pub struct ModdedDisc {
    inner: Box<dyn Disc>,
    files: Vec<FileEntry>,
    fst: Vec<u8>,
    replacements: Vec<Replacement>,
    /// Requested replacements for files that are not on the disc (ignored).
    pub unmatched: Vec<String>,
    open: Option<(usize, File)>,
}

impl ModdedDisc {
    /// `replacements`: `(path on the disc, host file)`. Only files already on the disc can be replaced:
    /// adding files would change the FST's size, which the guest reads from its header.
    pub fn new(
        mut inner: Box<dyn Disc>,
        replacements: &[(String, PathBuf)],
    ) -> Result<ModdedDisc, DiscError> {
        let fst = inner
            .sys_files()?
            .into_iter()
            .find(|(n, _)| *n == "fst.bin")
            .map(|(_, d)| d)
            .ok_or_else(|| DiscError::BadImage("no FST".into()))?;
        let mut next = inner.image_len().next_multiple_of(REPLACEMENT_ALIGN);
        let (mut moves, mut placed, mut unmatched) = (Vec::new(), Vec::new(), Vec::new());
        for (path, host) in replacements {
            if !inner.files().iter().any(|f| f.path == *path) {
                unmatched.push(path.clone());
                continue;
            }
            let len = std::fs::metadata(host).map_err(io_err(host))?.len();
            moves.push((path.clone(), next, len));
            placed.push(Replacement {
                start: next,
                len,
                host: host.clone(),
            });
            next = (next + len).next_multiple_of(REPLACEMENT_ALIGN);
        }
        let fst = rewrite_fst(&fst, &moves)?;
        let mut files = parse_fst(&fst)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(ModdedDisc {
            inner,
            files,
            fst,
            replacements: placed,
            unmatched,
            open: None,
        })
    }

    /// How many files are served from the host.
    pub fn replaced(&self) -> usize {
        self.replacements.len()
    }

    /// Read from the replacement `i` at `at` (inside it) into `dst`; what the file lacks stays zero.
    fn read_replacement(&mut self, i: usize, at: u64, dst: &mut [u8]) -> Result<(), DiscError> {
        let host = &self.replacements[i].host;
        if self.open.as_ref().is_none_or(|(n, _)| *n != i) {
            self.open = Some((i, File::open(host).map_err(io_err(host))?));
        }
        let f = &mut self.open.as_mut().expect("opened above").1;
        f.seek(SeekFrom::Start(at)).map_err(io_err(host))?;
        let mut got = 0;
        while got < dst.len() {
            match f.read(&mut dst[got..]).map_err(io_err(host))? {
                0 => break,
                n => got += n,
            }
        }
        Ok(())
    }
}

impl Disc for ModdedDisc {
    fn game_id(&self) -> &str {
        self.inner.game_id()
    }

    fn sys_files(&mut self) -> Result<Vec<(&'static str, Vec<u8>)>, DiscError> {
        let mut sys = self.inner.sys_files()?;
        for (name, data) in &mut sys {
            if *name == "fst.bin" {
                *data = self.fst.clone();
            }
        }
        Ok(sys)
    }

    fn dol(&mut self) -> Result<Vec<u8>, DiscError> {
        self.inner.dol()
    }

    fn files(&self) -> &[FileEntry] {
        &self.files
    }

    fn read_file(&mut self, entry: &FileEntry, out: &mut dyn Write) -> Result<(), DiscError> {
        let Some(i) = self
            .replacements
            .iter()
            .position(|r| r.start == entry.offset)
        else {
            return self.inner.read_file(entry, out);
        };
        let host = &self.replacements[i].host;
        let mut f = File::open(host).map_err(io_err(host))?;
        io::copy(&mut f, out).map_err(io_err(host))?;
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DiscError> {
        let end = offset + buf.len() as u64;
        let real = self.inner.image_len();
        // the part inside the real image is the wrapped disc's; the rest reads as zero unless a replacement is there
        let inside = (real.saturating_sub(offset) as usize).min(buf.len());
        self.inner.read_at(offset, &mut buf[..inside])?;
        buf[inside..].fill(0);
        let first = self
            .replacements
            .partition_point(|r| r.start + r.len <= offset);
        for i in first..self.replacements.len() {
            let (start, len) = (self.replacements[i].start, self.replacements[i].len);
            if start >= end {
                break;
            }
            let (lo, hi) = (offset.max(start), end.min(start + len));
            if lo < hi {
                let dst = &mut buf[(lo - offset) as usize..(hi - offset) as usize];
                self.read_replacement(i, lo - start, dst)?;
            }
        }
        Ok(())
    }

    fn image_len(&self) -> u64 {
        self.replacements
            .last()
            .map_or(self.inner.image_len(), |r| r.start + r.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FST of `/a.bin`, `/dir/b.bin`, `/dir/c.bin`, `/d.bin` with the given (offset, size) per file in that order.
    fn fst(files: [(u32, u32); 4]) -> Vec<u8> {
        let names = b"a.bin\0dir\0b.bin\0c.bin\0d.bin\0";
        let entry = |dir: bool, name: u32, a: u32, b: u32| {
            let mut e = Vec::new();
            e.extend_from_slice(&(name | ((dir as u32) << 24)).to_be_bytes());
            e.extend_from_slice(&a.to_be_bytes());
            e.extend_from_slice(&b.to_be_bytes());
            e
        };
        let mut t = entry(true, 0, 0, 6);
        t.extend(entry(false, 0, files[0].0, files[0].1)); // a.bin
        t.extend(entry(true, 6, 0, 5)); // dir
        t.extend(entry(false, 10, files[1].0, files[1].1)); // b.bin
        t.extend(entry(false, 16, files[2].0, files[2].1)); // c.bin
        t.extend(entry(false, 22, files[3].0, files[3].1)); // d.bin
        t.extend_from_slice(names);
        t
    }

    /// An image in memory: byte `i` is `i % 251`; `files()` from its FST.
    struct MemDisc {
        fst: Vec<u8>,
        files: Vec<FileEntry>,
        len: u64,
        reads: Vec<(u64, usize)>,
    }

    impl MemDisc {
        fn new() -> MemDisc {
            let fst = fst([
                (0x1000, 0x100),
                (0x2000, 0x200),
                (0x3000, 0x300),
                (0x4000, 0x10),
            ]);
            MemDisc {
                files: parse_fst(&fst).unwrap(),
                fst,
                len: 0x5000,
                reads: Vec::new(),
            }
        }
    }

    impl Disc for MemDisc {
        fn game_id(&self) -> &str {
            "GFEP01"
        }
        fn sys_files(&mut self) -> Result<Vec<(&'static str, Vec<u8>)>, DiscError> {
            Ok(vec![
                ("boot.bin", vec![1; 4]),
                ("fst.bin", self.fst.clone()),
                ("main.dol", vec![2; 4]),
            ])
        }
        fn dol(&mut self) -> Result<Vec<u8>, DiscError> {
            Ok(vec![2; 4])
        }
        fn files(&self) -> &[FileEntry] {
            &self.files
        }
        fn read_file(&mut self, entry: &FileEntry, out: &mut dyn Write) -> Result<(), DiscError> {
            let mut b = vec![0; entry.size as usize];
            self.read_at(entry.offset, &mut b)?;
            out.write_all(&b).unwrap();
            Ok(())
        }
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DiscError> {
            self.reads.push((offset, buf.len()));
            for (i, b) in buf.iter_mut().enumerate() {
                let at = offset + i as u64;
                *b = if at < self.len { (at % 251) as u8 } else { 0 };
            }
            Ok(())
        }
        fn image_len(&self) -> u64 {
            self.len
        }
    }

    fn files_of(fst: &[u8]) -> Vec<(String, u64, u64)> {
        parse_fst(fst)
            .unwrap()
            .into_iter()
            .map(|e| (e.path, e.offset, e.size))
            .collect()
    }

    #[test]
    fn rewrite_fst_changes_only_the_named_entries() {
        let original = fst([
            (0x1000, 0x100),
            (0x2000, 0x200),
            (0x3000, 0x300),
            (0x4000, 0x10),
        ]);
        let out = rewrite_fst(
            &original,
            &[
                ("dir/c.bin".into(), 0x9000, 0x44),
                ("d.bin".into(), 0xA000, 0),
            ],
        )
        .unwrap();
        assert_eq!(out.len(), original.len());
        assert_eq!(
            files_of(&out),
            [
                ("a.bin".to_string(), 0x1000, 0x100),
                ("dir/b.bin".to_string(), 0x2000, 0x200),
                ("dir/c.bin".to_string(), 0x9000, 0x44),
                ("d.bin".to_string(), 0xA000, 0),
            ]
        );
        // names, directory entries and everything else are byte for byte the same
        let diff: Vec<usize> = (0..out.len()).filter(|&i| out[i] != original[i]).collect();
        assert!(diff.iter().all(|&i| (48..72).contains(&i)), "{diff:?}");
        assert!(rewrite_fst(&original, &[("nope".into(), 0, 0)]).is_err());
        assert!(rewrite_fst(&original, &[("a.bin".into(), 1 << 32, 0)]).is_err());
    }

    #[test]
    fn modded_disc_serves_replacements_after_the_image_and_the_rest_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let (big, small) = (dir.path().join("big"), dir.path().join("small"));
        let big_data: Vec<u8> = (0..0x250u32).map(|i| (i % 7) as u8 + 100).collect();
        std::fs::write(&big, &big_data).unwrap();
        std::fs::write(&small, b"hello").unwrap();
        let mut disc = ModdedDisc::new(
            Box::new(MemDisc::new()),
            &[
                ("dir/b.bin".into(), big),
                ("d.bin".into(), small),
                ("missing.bin".into(), dir.path().join("x")),
            ],
        )
        .unwrap();
        assert_eq!(disc.unmatched, ["missing.bin"]);
        assert_eq!(disc.replaced(), 2);

        // the guest's FST: replaced files point after the image, 32-byte aligned, others unchanged
        let sys = disc.sys_files().unwrap();
        let fst = &sys.iter().find(|(n, _)| *n == "fst.bin").unwrap().1;
        assert_eq!(
            files_of(fst),
            [
                ("a.bin".to_string(), 0x1000, 0x100),
                ("dir/b.bin".to_string(), 0x5000, 0x250),
                ("dir/c.bin".to_string(), 0x3000, 0x300),
                ("d.bin".to_string(), 0x5260, 5),
            ]
        );
        assert_eq!(
            sys.iter().find(|(n, _)| *n == "boot.bin").unwrap().1,
            [1; 4]
        );
        assert_eq!(
            disc.files()
                .iter()
                .find(|f| f.path == "d.bin")
                .unwrap()
                .offset,
            0x5260
        );
        assert_eq!(disc.image_len(), 0x5265);

        // a read of the replaced offsets comes from the host file, also from the middle and across the end
        let mut buf = vec![0u8; 0x250];
        disc.read_at(0x5000, &mut buf).unwrap();
        assert_eq!(buf, big_data);
        let mut buf = [0u8; 8];
        disc.read_at(0x5010, &mut buf).unwrap();
        assert_eq!(buf, big_data[0x10..0x18]);
        let mut buf = [0u8; 0x40];
        disc.read_at(0x5240, &mut buf).unwrap();
        assert_eq!(&buf[..0x10], &big_data[0x240..]);
        assert_eq!(
            &buf[0x10..0x20],
            [0; 0x10],
            "the gap between files reads as zero"
        );
        assert_eq!(&buf[0x20..0x25], b"hello");

        // every other read is the wrapped disc's, byte for byte; one that starts inside the image and runs past
        // its end gets the image's bytes and then the replacement
        let mut buf = vec![0u8; 0x300];
        disc.read_at(0x3000, &mut buf).unwrap();
        assert!(buf
            .iter()
            .enumerate()
            .all(|(i, &b)| b == ((0x3000 + i) % 251) as u8));
        let mut buf = [0xFFu8; 0x20];
        disc.read_at(0x4FF0, &mut buf).unwrap();
        assert!(buf[..0x10]
            .iter()
            .enumerate()
            .all(|(i, &b)| b == ((0x4FF0 + i) % 251) as u8));
        assert_eq!(&buf[0x10..], &big_data[..0x10]);

        // read_file follows the new entries
        let mut out = Vec::new();
        let b = disc
            .files()
            .iter()
            .find(|f| f.path == "dir/b.bin")
            .unwrap()
            .clone();
        disc.read_file(&b, &mut out).unwrap();
        assert_eq!(out, big_data);
        let mut out = Vec::new();
        let a = disc
            .files()
            .iter()
            .find(|f| f.path == "a.bin")
            .unwrap()
            .clone();
        disc.read_file(&a, &mut out).unwrap();
        assert_eq!(out.len(), 0x100);
        assert_eq!(out[0], (0x1000 % 251) as u8);
    }

    #[test]
    fn no_replacement_changes_nothing() {
        let mut disc = ModdedDisc::new(Box::new(MemDisc::new()), &[]).unwrap();
        let sys = disc.sys_files().unwrap();
        assert_eq!(
            sys.iter().find(|(n, _)| *n == "fst.bin").unwrap().1,
            MemDisc::new().fst
        );
        assert_eq!(disc.image_len(), 0x5000);
        let mut buf = [0u8; 4];
        disc.read_at(0x5000, &mut buf).unwrap();
        assert_eq!(buf, [0; 4]);
    }
}
