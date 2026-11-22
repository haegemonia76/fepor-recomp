//! Disc image containers. A plain GCM (ISO), a GCZ and an RVZ all present the same thing: the bytes of the
//! GameCube disc at any offset. [`IsoDisc`](crate::disc::IsoDisc) reads the disc file system through this.
//!
//! - GCZ: Dolphin's older format; zlib blocks of a fixed size behind a table of block offsets.
//! - RVZ: Dolphin's current format (<https://github.com/dolphin-emu/dolphin/blob/master/docs/WiaAndRvz.md>),
//!   GameCube discs only: Zstandard (or no) compression per chunk, and the pseudo-random padding of the disc
//!   stored as a seed and regenerated here, so the image reads back byte for byte like the ISO.
//!   WIA, the format RVZ derives from, and Wii discs are not supported.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::disc::DiscError;

/// A disc image as random-access bytes.
pub trait Image {
    /// Size of the disc image in bytes.
    fn len(&self) -> u64;
    /// Fill `buf` from `offset`; what lies beyond the end of the image reads as zero.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

const GCZ_MAGIC: [u8; 4] = 0xB10B_C001u32.to_le_bytes();
const RVZ_MAGIC: &[u8; 4] = b"RVZ\x01";
const WIA_MAGIC: &[u8; 4] = b"WIA\x01";

/// Open `path` as whatever image format its first bytes say it is (GCM when none matches).
pub fn open(path: &Path) -> Result<Box<dyn Image>, DiscError> {
    let io_err = |e| DiscError::Io(path.to_path_buf(), e);
    let mut file = File::open(path).map_err(io_err)?;
    let mut magic = [0u8; 4];
    let n = file.read(&mut magic).map_err(io_err)?;
    file.seek(SeekFrom::Start(0)).map_err(io_err)?;
    let bad = |e: String| DiscError::BadImage(e);
    if n == 4 && magic == GCZ_MAGIC {
        return Ok(Box::new(GczImage::open(file).map_err(|e| match e {
            ImageError::Io(e) => io_err(e),
            ImageError::Bad(m) => bad(format!("GCZ: {m}")),
        })?));
    }
    if n == 4 && &magic == RVZ_MAGIC {
        return Ok(Box::new(RvzImage::open(file).map_err(|e| match e {
            ImageError::Io(e) => io_err(e),
            ImageError::Bad(m) => bad(format!("RVZ: {m}")),
        })?));
    }
    if n == 4 && &magic == WIA_MAGIC {
        return Err(bad(
            "WIA images are not supported (convert to RVZ or ISO)".into()
        ));
    }
    let len = file.metadata().map_err(io_err)?.len();
    Ok(Box::new(FileImage { file, len }))
}

enum ImageError {
    Io(io::Error),
    Bad(String),
}

impl From<io::Error> for ImageError {
    fn from(e: io::Error) -> Self {
        ImageError::Io(e)
    }
}

fn bad<T>(m: impl Into<String>) -> Result<T, ImageError> {
    Err(ImageError::Bad(m.into()))
}

fn corrupt(m: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.into())
}

/// A plain GCM file.
struct FileImage {
    file: File,
    len: u64,
}

impl Image for FileImage {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        buf.fill(0);
        if offset >= self.len {
            return Ok(());
        }
        let n = buf.len().min((self.len - offset) as usize);
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut buf[..n])
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

fn read_exact_at(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut v = vec![0u8; len];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut v)?;
    Ok(v)
}

/// Copy `block` (the data of the block starting at `block_start`) into the part of `buf` it covers.
/// Returns how many bytes of `buf` were filled from `offset` on.
fn copy_from_block(block: &[u8], block_start: u64, offset: u64, buf: &mut [u8]) -> usize {
    let at = (offset - block_start) as usize;
    let n = buf.len().min(block.len().saturating_sub(at));
    buf[..n].copy_from_slice(&block[at..at + n]);
    n
}

// ---- GCZ ------------------------------------------------------------------------------------------------

/// Header (little endian): magic, sub type, compressed size (u64), data size (u64), block size, block count;
/// then one u64 per block (offset in the data area, top bit set when stored uncompressed), one adler32 per
/// block, then the data.
struct GczImage {
    file: File,
    size: u64,
    block_size: u64,
    data_start: u64,
    compressed_size: u64,
    pointers: Vec<u64>,
    cache: Option<(usize, Vec<u8>)>,
}

impl GczImage {
    fn open(mut file: File) -> Result<GczImage, ImageError> {
        let head = read_exact_at(&mut file, 0, 32)?;
        let (compressed_size, size) = (le64(&head, 8), le64(&head, 16));
        let (block_size, n) = (le32(&head, 24) as u64, le32(&head, 28) as usize);
        if block_size == 0 || n == 0 || size.div_ceil(block_size) != n as u64 {
            return bad("inconsistent header");
        }
        let table = read_exact_at(&mut file, 32, n * 8)?;
        let pointers = (0..n).map(|i| le64(&table, i * 8)).collect();
        Ok(GczImage {
            file,
            size,
            block_size,
            data_start: 32 + n as u64 * 12,
            compressed_size,
            pointers,
            cache: None,
        })
    }

    fn block(&mut self, i: usize) -> io::Result<&[u8]> {
        if self.cache.as_ref().is_none_or(|(c, _)| *c != i) {
            const UNCOMPRESSED: u64 = 1 << 63;
            let p = self.pointers[i];
            let start = p & !UNCOMPRESSED;
            let end = self
                .pointers
                .get(i + 1)
                .map_or(self.compressed_size, |n| n & !UNCOMPRESSED);
            let len = end
                .checked_sub(start)
                .ok_or_else(|| corrupt("block table out of order"))?;
            let raw = read_exact_at(&mut self.file, self.data_start + start, len as usize)?;
            let data = if p & UNCOMPRESSED != 0 {
                raw
            } else {
                let mut out = Vec::with_capacity(self.block_size as usize);
                flate2::read::ZlibDecoder::new(&raw[..]).read_to_end(&mut out)?;
                out
            };
            self.cache = Some((i, data));
        }
        Ok(&self.cache.as_ref().expect("block cached").1)
    }
}

impl Image for GczImage {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_at(&mut self, mut offset: u64, buf: &mut [u8]) -> io::Result<()> {
        buf.fill(0);
        let mut done = 0;
        while done < buf.len() && offset < self.size {
            let i = (offset / self.block_size) as usize;
            let start = i as u64 * self.block_size;
            let block = self.block(i)?;
            let n = copy_from_block(block, start, offset, &mut buf[done..]);
            if n == 0 {
                return Err(corrupt("block shorter than the table says"));
            }
            done += n;
            offset += n as u64;
        }
        Ok(())
    }
}

// ---- RVZ ------------------------------------------------------------------------------------------------

const RVZ_ZSTD: u32 = 5;
const RVZ_NONE: u32 = 0;

/// A run of the disc stored as groups (`wia_raw_data_t`).
struct RawData {
    offset: u64,
    size: u64,
    first_group: usize,
    groups: usize,
}

/// One compressed chunk (`rvz_group_t`).
#[derive(Clone, Copy)]
struct Group {
    offset: u64,
    size: usize,
    compressed: bool,
    packed_size: u32,
}

struct RvzImage {
    file: File,
    size: u64,
    compression: u32,
    chunk_size: u64,
    head: [u8; 0x80],
    raw: Vec<RawData>,
    groups: Vec<Group>,
    cache: Option<(usize, Vec<u8>)>,
}

fn decompress(method: u32, data: &[u8], expected: usize) -> io::Result<Vec<u8>> {
    match method {
        RVZ_NONE => Ok(data.to_vec()),
        RVZ_ZSTD => {
            let mut out = Vec::with_capacity(expected);
            ruzstd::decoding::StreamingDecoder::new(data)
                .map_err(|e| corrupt(format!("zstd: {e}")))?
                .read_to_end(&mut out)?;
            Ok(out)
        }
        m => Err(corrupt(format!("compression method {m} is not supported"))),
    }
}

impl RvzImage {
    fn open(mut file: File) -> Result<RvzImage, ImageError> {
        let head = read_exact_at(&mut file, 0, 0x48 + 0xDC)?;
        let size = be64(&head, 0x24);
        let d = &head[0x48..];
        if be32(d, 0) != 1 {
            return bad("not a GameCube disc (Wii discs are not supported)");
        }
        let compression = be32(d, 4);
        if compression != RVZ_NONE && compression != RVZ_ZSTD {
            return bad(format!("compression method {compression} is not supported"));
        }
        let chunk_size = be32(d, 0xC) as u64;
        if chunk_size == 0 {
            return bad("chunk size 0");
        }
        let mut dhead = [0u8; 0x80];
        dhead.copy_from_slice(&d[0x10..0x90]);
        let (n_raw, raw_off, raw_len) = (
            be32(d, 0xB4) as usize,
            be64(d, 0xB8),
            be32(d, 0xC0) as usize,
        );
        let (n_groups, group_off, group_len) = (
            be32(d, 0xC4) as usize,
            be64(d, 0xC8),
            be32(d, 0xD0) as usize,
        );
        let table =
            |file: &mut File, off, len, entry: usize, n: usize| -> Result<Vec<u8>, ImageError> {
                let packed = read_exact_at(file, off, len)?;
                let t = decompress(compression, &packed, n * entry)?;
                if t.len() < n * entry {
                    return bad("table shorter than its entry count");
                }
                Ok(t)
            };
        let t = table(&mut file, raw_off, raw_len, 0x18, n_raw)?;
        let raw = (0..n_raw)
            .map(|i| {
                let e = &t[i * 0x18..];
                let (mut offset, mut length) = (be64(e, 0), be64(e, 8));
                // the first entry starts 0x80 into the disc but really holds the whole first 32 KiB
                let aligned = offset & !0x7FFF;
                length += offset - aligned;
                offset = aligned;
                RawData {
                    offset,
                    size: length,
                    first_group: be32(e, 16) as usize,
                    groups: be32(e, 20) as usize,
                }
            })
            .collect();
        let t = table(&mut file, group_off, group_len, 12, n_groups)?;
        let groups = (0..n_groups)
            .map(|i| {
                let e = &t[i * 12..];
                let ds = be32(e, 4);
                Group {
                    offset: be32(e, 0) as u64 * 4,
                    size: (ds & 0x7FFF_FFFF) as usize,
                    compressed: ds >> 31 != 0,
                    packed_size: be32(e, 8),
                }
            })
            .collect();
        Ok(RvzImage {
            file,
            size,
            compression,
            chunk_size,
            head: dhead,
            raw,
            groups,
            cache: None,
        })
    }

    /// The `len` bytes of group `g`, which starts at disc offset `disc_offset`.
    fn group(&mut self, g: usize, disc_offset: u64, len: usize) -> io::Result<&[u8]> {
        if self.cache.as_ref().is_none_or(|(c, _)| *c != g) {
            let grp = *self
                .groups
                .get(g)
                .ok_or_else(|| corrupt("group index out of range"))?;
            let mut data = if grp.size == 0 {
                vec![0u8; len]
            } else {
                let raw = read_exact_at(&mut self.file, grp.offset, grp.size)?;
                let method = if grp.compressed {
                    self.compression
                } else {
                    RVZ_NONE
                };
                let d = decompress(method, &raw, len)?;
                if grp.packed_size != 0 {
                    unpack(&d, len, disc_offset)?
                } else {
                    d
                }
            };
            data.resize(len, 0);
            self.cache = Some((g, data));
        }
        Ok(&self.cache.as_ref().expect("group cached").1)
    }
}

impl Image for RvzImage {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_at(&mut self, mut offset: u64, buf: &mut [u8]) -> io::Result<()> {
        buf.fill(0);
        let mut done = 0;
        while done < buf.len() && offset < self.size {
            if offset < 0x80 {
                let n = buf[done..].len().min(0x80 - offset as usize);
                buf[done..done + n]
                    .copy_from_slice(&self.head[offset as usize..offset as usize + n]);
                done += n;
                offset += n as u64;
                continue;
            }
            let Some(r) = self
                .raw
                .iter()
                .find(|r| (r.offset..r.offset + r.size).contains(&offset))
            else {
                // not stored: zeros up to the next stored run
                let next = self
                    .raw
                    .iter()
                    .map(|r| r.offset)
                    .filter(|&o| o > offset)
                    .min()
                    .unwrap_or(self.size);
                let n = (next - offset).min((buf.len() - done) as u64) as usize;
                done += n;
                offset += n as u64;
                continue;
            };
            let (r_offset, r_size, first) = (r.offset, r.size, r.first_group);
            let k = (offset - r_offset) / self.chunk_size;
            if k as usize >= r.groups {
                return Err(corrupt("offset beyond the groups of its run"));
            }
            let start = r_offset + k * self.chunk_size;
            let len = self.chunk_size.min(r_size - k * self.chunk_size) as usize;
            let chunk = self.group(first + k as usize, start, len)?;
            let n = copy_from_block(chunk, start, offset, &mut buf[done..]);
            if n == 0 {
                return Err(corrupt("empty chunk"));
            }
            done += n;
            offset += n as u64;
        }
        Ok(())
    }
}

/// Decode RVZ packing: runs of stored bytes and runs of the disc's pseudo-random padding (a seed and a
/// length). `disc_offset` is where the data starts on the disc: the generator is aligned to 32 KiB sectors.
fn unpack(data: &[u8], len: usize, disc_offset: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(len);
    let mut p = 0;
    while p + 4 <= data.len() {
        let word = be32(data, p);
        p += 4;
        let n = (word & 0x7FFF_FFFF) as usize;
        if word >> 31 == 0 {
            let chunk = data
                .get(p..p + n)
                .ok_or_else(|| corrupt("packed run past the end"))?;
            out.extend_from_slice(chunk);
            p += n;
        } else {
            let seed = data
                .get(p..p + 68)
                .ok_or_else(|| corrupt("packed seed past the end"))?;
            p += 68;
            let mut lfg = Lfg::new(seed);
            lfg.skip((disc_offset + out.len() as u64) % 0x8000);
            out.extend((0..n).map(|_| lfg.byte()));
        }
    }
    Ok(out)
}

/// The lagged Fibonacci generator (xor, lags 32 and 521) that fills the padding of GameCube discs.
struct Lfg {
    buf: [u32; 521],
    /// Next byte to output, in `0..521 * 4`.
    pos: usize,
}

impl Lfg {
    fn new(seed: &[u8]) -> Lfg {
        let mut buf = [0u32; 521];
        for (i, w) in seed.chunks_exact(4).take(17).enumerate() {
            buf[i] = u32::from_be_bytes([w[0], w[1], w[2], w[3]]);
        }
        for i in 17..521 {
            buf[i] = (buf[i - 17] << 23) ^ (buf[i - 16] >> 9) ^ buf[i - 1];
        }
        let mut g = Lfg { buf, pos: 0 };
        for _ in 0..4 {
            g.advance();
        }
        g
    }

    /// Step the state by a whole buffer.
    fn advance(&mut self) {
        for i in 0..32 {
            self.buf[i] ^= self.buf[i + 521 - 32];
        }
        for i in 32..521 {
            self.buf[i] ^= self.buf[i - 32];
        }
    }

    fn skip(&mut self, n: u64) {
        const SPAN: u64 = 521 * 4;
        let total = self.pos as u64 + n;
        for _ in 0..total / SPAN {
            self.advance();
        }
        self.pos = (total % SPAN) as usize;
    }

    fn byte(&mut self) -> u8 {
        let w = self.buf[self.pos / 4];
        // the second byte is taken from bit 18, not 16
        let b = match self.pos % 4 {
            0 => (w >> 24) as u8,
            1 => (w >> 18) as u8,
            2 => (w >> 8) as u8,
            _ => w as u8,
        };
        self.pos += 1;
        if self.pos == 521 * 4 {
            self.advance();
            self.pos = 0;
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A disc-like byte pattern that does not compress to nothing.
    fn pattern(len: usize) -> Vec<u8> {
        let mut x = 0x1234_5678u32;
        (0..len)
            .map(|i| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                if i % 3 == 0 {
                    (x >> 24) as u8
                } else {
                    7
                }
            })
            .collect()
    }

    fn read_all(img: &mut dyn Image) -> Vec<u8> {
        let mut v = vec![0u8; img.len() as usize];
        img.read_at(0, &mut v).unwrap();
        v
    }

    fn write_gcz(path: &Path, data: &[u8], block: usize, store_second_raw: bool) {
        let blocks: Vec<&[u8]> = data.chunks(block).collect();
        let mut stored: Vec<(bool, Vec<u8>)> = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            if store_second_raw && i == 1 {
                stored.push((true, b.to_vec()));
            } else {
                let mut z =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                z.write_all(b).unwrap();
                stored.push((false, z.finish().unwrap()));
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(&GCZ_MAGIC);
        out.extend_from_slice(&0u32.to_le_bytes());
        let total: usize = stored.iter().map(|(_, s)| s.len()).sum();
        out.extend_from_slice(&(total as u64).to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(&(block as u32).to_le_bytes());
        out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
        let mut at = 0u64;
        for (raw, s) in &stored {
            out.extend_from_slice(&(at | if *raw { 1 << 63 } else { 0 }).to_le_bytes());
            at += s.len() as u64;
        }
        out.extend(std::iter::repeat_n(0u8, blocks.len() * 4)); // checksums, unchecked
        for (_, s) in &stored {
            out.extend_from_slice(s);
        }
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn gcz_reads_compressed_and_stored_blocks_across_block_borders() {
        let dir = tempfile::tempdir().unwrap();
        let data = pattern(5 * 4096 + 1000);
        let p = dir.path().join("a.gcz");
        write_gcz(&p, &data, 4096, true);
        let mut img = open(&p).unwrap();
        assert_eq!(img.len(), data.len() as u64);
        assert_eq!(read_all(&mut *img), data);
        let mut buf = vec![0u8; 5000];
        img.read_at(3000, &mut buf).unwrap();
        assert_eq!(buf, data[3000..8000]);
        // past the end reads as zero
        let mut tail = vec![9u8; 64];
        img.read_at(data.len() as u64 - 10, &mut tail).unwrap();
        assert_eq!(&tail[..10], &data[data.len() - 10..]);
        assert!(tail[10..].iter().all(|&b| b == 0));
    }

    /// The generator's first bytes for a known seed, computed by hand from the description of the algorithm:
    /// with an all-zero seed the whole state is zero and so is every output byte.
    #[test]
    fn lfg_zero_seed_outputs_zeros_and_skip_matches_reading() {
        let mut z = Lfg::new(&[0u8; 68]);
        assert!((0..5000).all(|_| z.byte() == 0));
        let seed: Vec<u8> = (0..68u8)
            .map(|i| i.wrapping_mul(37).wrapping_add(11))
            .collect();
        let mut a = Lfg::new(&seed);
        let first: Vec<u8> = (0..6000).map(|_| a.byte()).collect();
        let mut b = Lfg::new(&seed);
        b.skip(4321);
        assert_eq!(b.byte(), first[4321]);
        assert_eq!(b.byte(), first[4322]);
        assert!(first.iter().any(|&x| x != 0));
    }

    /// Build a GameCube RVZ of `data`: one raw run, `chunk` byte groups, every second group zstd-compressed
    /// (the others stored), and the last group made of packed padding.
    fn write_rvz(path: &Path, data: &[u8], chunk: usize, zstd: bool, packed_tail: Option<&[u8]>) {
        let be = |v: u64, n: usize| v.to_be_bytes()[8 - n..].to_vec();
        let groups: Vec<&[u8]> = data.chunks(chunk).collect();
        let mut file = vec![0u8; 0x48 + 0xDC];
        let mut entries = Vec::new(); // (offset, size, compressed, packed)
        for (i, g) in groups.iter().enumerate() {
            let last = i + 1 == groups.len();
            let (bytes, compressed, packed): (Vec<u8>, bool, u32) =
                if let (true, Some(seed)) = (last, packed_tail) {
                    // the whole last group is padding drawn from `seed`
                    let mut p = Vec::new();
                    p.extend_from_slice(&(g.len() as u32 | 0x8000_0000).to_be_bytes());
                    p.extend_from_slice(seed);
                    (p.clone(), false, p.len() as u32)
                } else if zstd && i % 2 == 0 {
                    let c = ruzstd::encoding::compress_to_vec(
                        *g,
                        ruzstd::encoding::CompressionLevel::Fastest,
                    );
                    (c, true, 0)
                } else {
                    (g.to_vec(), false, 0)
                };
            while file.len() % 4 != 0 {
                file.push(0);
            }
            entries.push((file.len() as u64, bytes.len(), compressed, packed));
            file.extend_from_slice(&bytes);
        }
        let mut raw = Vec::new();
        raw.extend(be(0x80, 8));
        raw.extend(be(data.len() as u64 - 0x80, 8));
        raw.extend(be(0, 4));
        raw.extend(be(groups.len() as u64, 4));
        let mut grp = Vec::new();
        for (off, len, c, packed) in &entries {
            grp.extend(be(off / 4, 4));
            grp.extend(be(*len as u64 | if *c { 0x8000_0000 } else { 0 }, 4));
            grp.extend(be(*packed as u64, 4));
        }
        let (raw_c, grp_c) = if zstd {
            let z = |d: &[u8]| {
                ruzstd::encoding::compress_to_vec(d, ruzstd::encoding::CompressionLevel::Fastest)
            };
            (z(&raw), z(&grp))
        } else {
            (raw.clone(), grp.clone())
        };
        let raw_off = file.len() as u64;
        file.extend_from_slice(&raw_c);
        let grp_off = file.len() as u64;
        file.extend_from_slice(&grp_c);
        file[..4].copy_from_slice(RVZ_MAGIC);
        file[0x24..0x2C].copy_from_slice(&(data.len() as u64).to_be_bytes());
        let d = &mut file[0x48..];
        d[0..4].copy_from_slice(&1u32.to_be_bytes());
        d[4..8].copy_from_slice(&(if zstd { RVZ_ZSTD } else { RVZ_NONE }).to_be_bytes());
        d[0xC..0x10].copy_from_slice(&(chunk as u32).to_be_bytes());
        d[0x10..0x90].copy_from_slice(&data[..0x80]);
        d[0xB4..0xB8].copy_from_slice(&1u32.to_be_bytes());
        d[0xB8..0xC0].copy_from_slice(&raw_off.to_be_bytes());
        d[0xC0..0xC4].copy_from_slice(&(raw_c.len() as u32).to_be_bytes());
        d[0xC4..0xC8].copy_from_slice(&(groups.len() as u32).to_be_bytes());
        d[0xC8..0xD0].copy_from_slice(&grp_off.to_be_bytes());
        d[0xD0..0xD4].copy_from_slice(&(grp_c.len() as u32).to_be_bytes());
        std::fs::write(path, file).unwrap();
    }

    #[test]
    fn rvz_reads_stored_and_zstd_groups_and_the_header_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let data = pattern(0x8000 * 5 + 777);
        for zstd in [false, true] {
            let p = dir.path().join(format!("a{zstd}.rvz"));
            write_rvz(&p, &data, 0x8000, zstd, None);
            let mut img = open(&p).unwrap();
            assert_eq!(read_all(&mut *img), data, "zstd={zstd}");
            let mut buf = vec![0u8; 40000];
            img.read_at(0x7F00, &mut buf).unwrap();
            assert_eq!(buf, data[0x7F00..0x7F00 + 40000]);
        }
    }

    #[test]
    fn rvz_regenerates_packed_padding_at_its_sector_offset() {
        let dir = tempfile::tempdir().unwrap();
        let seed: Vec<u8> = (0..68u8)
            .map(|i| i.wrapping_mul(91).wrapping_add(5))
            .collect();
        // the last group starts 0x8000 * 4 into the disc: a sector border, so the generator starts fresh
        let data = pattern(0x8000 * 5);
        let p = dir.path().join("p.rvz");
        write_rvz(&p, &data, 0x8000, true, Some(&seed));
        let mut img = open(&p).unwrap();
        let all = read_all(&mut *img);
        assert_eq!(all[..0x8000 * 4], data[..0x8000 * 4]);
        let mut lfg = Lfg::new(&seed);
        let want: Vec<u8> = (0..0x8000).map(|_| lfg.byte()).collect();
        assert_eq!(all[0x8000 * 4..], want[..]);
    }

    /// Opt-in check against images made by Dolphin: `FEPOR_TEST_IMAGE=<.rvz|.gcz>` and
    /// `FEPOR_TEST_ISO=<the same disc as an ISO, made by another tool>`; every byte must agree.
    /// `cargo test -p fepor-core --release real_image -- --ignored`
    #[test]
    #[ignore]
    fn real_image_reads_like_its_iso() {
        let (Ok(image), Ok(iso)) = (
            std::env::var("FEPOR_TEST_IMAGE"),
            std::env::var("FEPOR_TEST_ISO"),
        ) else {
            panic!("set FEPOR_TEST_IMAGE and FEPOR_TEST_ISO");
        };
        let mut a = open(Path::new(&image)).unwrap();
        let mut b = open(Path::new(&iso)).unwrap();
        assert_eq!(a.len(), b.len());
        let mut at = 0u64;
        let (mut x, mut y) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
        while at < a.len() {
            let n = x.len().min((a.len() - at) as usize);
            a.read_at(at, &mut x[..n]).unwrap();
            b.read_at(at, &mut y[..n]).unwrap();
            if x[..n] != y[..n] {
                let d = (0..n).find(|&i| x[i] != y[i]).unwrap();
                panic!("first difference at disc offset {:#x}", at + d as u64);
            }
            at += n as u64;
        }
    }

    #[test]
    fn unsupported_containers_are_refused_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("w.wia");
        std::fs::write(&p, b"WIA\x01 and some more bytes").unwrap();
        let e = open(&p).err().expect("refused").to_string();
        assert!(e.contains("WIA"), "{e}");
    }
}
