//! Abstract disc reader: raw ISO/GCM and extracted folder (`sys/` + `files/`, Dolphin layout).
//! New image formats (CISO, RVZ, ...) implement [`Disc`].

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::dol;

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
    let bad = |m: &str| DiscError::BadImage(format!("FST: {m}"));
    if fst.len() < 12 || fst[0] != 1 {
        return Err(bad("missing root directory"));
    }
    let count = be32(fst, 8) as usize;
    let table = count.checked_mul(12).filter(|&t| t <= fst.len()).ok_or_else(|| bad("truncated"))?;
    let names = &fst[table..];
    let name_at = |off: usize| -> Result<String, DiscError> {
        let tail = names.get(off..).ok_or_else(|| bad("name offset out of range"))?;
        let end = tail.iter().position(|&b| b == 0).ok_or_else(|| bad("unterminated name"))?;
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
            out.push(FileEntry { path, offset: be32(e, 4) as u64, size: be32(e, 8) as u64 });
        }
    }
    Ok(out)
}

pub struct IsoDisc {
    path: PathBuf,
    file: File,
    id: String,
    boot: Vec<u8>,
    files: Vec<FileEntry>,
}

impl IsoDisc {
    pub fn open(path: &Path) -> Result<Self, DiscError> {
        let mut file = File::open(path).map_err(io_err(path))?;
        let mut boot = vec![0u8; BOOT_BIN_SIZE];
        file.read_exact(&mut boot)
            .map_err(|_| DiscError::BadImage("file too small".into()))?;
        if be32(&boot, 0x1C) != GCM_MAGIC {
            return Err(DiscError::BadImage("GCM magic not found (compressed formats are unsupported)".into()));
        }
        let id = game_id_of(&boot)?;
        let mut disc = IsoDisc { path: path.to_path_buf(), file, id, boot, files: Vec::new() };
        let fst = disc.fst()?;
        disc.files = parse_fst(&fst)?;
        Ok(disc)
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, DiscError> {
        let mut buf = vec![0u8; len];
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(&mut buf))
            .map_err(io_err(&self.path))?;
        Ok(buf)
    }

    fn fst(&mut self) -> Result<Vec<u8>, DiscError> {
        let (off, len) = (be32(&self.boot, 0x424) as u64, be32(&self.boot, 0x428) as usize);
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
        let apploader_len = APPLOADER_HEADER + be32(&hdr, 0x14) as usize + be32(&hdr, 0x18) as usize;
        Ok(vec![
            ("boot.bin", self.boot.clone()),
            ("bi2.bin", self.read_at(BI2_OFFSET, BI2_SIZE)?),
            ("apploader.img", self.read_at(APPLOADER_OFFSET, apploader_len)?),
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
        self.file.seek(SeekFrom::Start(entry.offset)).map_err(io_err(&self.path))?;
        let copied = io::copy(&mut (&mut self.file).take(entry.size), out).map_err(io_err(&self.path))?;
        if copied != entry.size {
            return Err(DiscError::BadImage(format!("{} is truncated", entry.path)));
        }
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), DiscError> {
        buf.fill(0);
        let len = self.file.metadata().map_err(io_err(&self.path))?.len();
        if offset >= len {
            return Ok(());
        }
        let n = buf.len().min((len - offset) as usize);
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(&mut buf[..n]))
            .map_err(io_err(&self.path))
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
        let mut files = match std::fs::read(sys.join("fst.bin")).ok().and_then(|f| parse_fst(&f).ok()) {
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
            (BI2_OFFSET, size_of(&sys.join("bi2.bin")), sys.join("bi2.bin")),
            (APPLOADER_OFFSET, size_of(&sys.join("apploader.img")), sys.join("apploader.img")),
            (word(0x424), size_of(&sys.join("fst.bin")), sys.join("fst.bin")),
            (word(0x420), size_of(&sys.join("main.dol")), sys.join("main.dol")),
        ];
        for f in &files {
            regions.push((f.offset, f.size, root.join("files").join(&f.path)));
        }
        Ok(FolderDisc { root: root.to_path_buf(), id, files, regions })
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
            out.push(FileEntry { path: rel, offset: 0, size: meta.len() });
        }
    }
    Ok(())
}

impl Disc for FolderDisc {
    fn game_id(&self) -> &str {
        &self.id
    }

    fn sys_files(&mut self) -> Result<Vec<(&'static str, Vec<u8>)>, DiscError> {
        ["boot.bin", "bi2.bin", "apploader.img", "fst.bin", "main.dol"]
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
        for (start, len, path) in &self.regions {
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
}
