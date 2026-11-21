//! Minimal DOL header parsing (size, entry point).

pub const HEADER_SIZE: usize = 0x100;
const SECTIONS: usize = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DolInfo {
    pub entry: u32,
    /// Total file size implied by the header (end of the furthest section).
    pub size: usize,
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes(b[off..off + 4].try_into().unwrap())
}

/// Parse a DOL header. Returns `None` if the buffer is shorter than the header.
pub fn parse_header(header: &[u8]) -> Option<DolInfo> {
    if header.len() < HEADER_SIZE {
        return None;
    }
    let mut size = HEADER_SIZE;
    for i in 0..SECTIONS {
        let off = be32(header, i * 4) as usize;
        let len = be32(header, 0x90 + i * 4) as usize;
        if len != 0 {
            size = size.max(off.checked_add(len)?);
        }
    }
    Some(DolInfo {
        entry: be32(header, 0xE0),
        size,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum DolError {
    #[error("DOL is shorter than its header")]
    TooShort,
    #[error("DOL section {0} lies outside the file")]
    BadSection(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Text,
    Data,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    pub kind: SectionKind,
    pub addr: u32,
    pub size: u32,
    /// Offset in the DOL file.
    pub offset: u32,
}

/// A parsed DOL: non-empty sections (7 text then 11 data slots), entry point and BSS range.
#[derive(Debug, Clone)]
pub struct Dol {
    pub entry: u32,
    pub bss_addr: u32,
    pub bss_size: u32,
    pub sections: Vec<Section>,
    bytes: Vec<u8>,
}

impl Dol {
    pub fn parse(bytes: Vec<u8>) -> Result<Dol, DolError> {
        if bytes.len() < HEADER_SIZE {
            return Err(DolError::TooShort);
        }
        let mut sections = Vec::new();
        for i in 0..SECTIONS {
            let (offset, addr, size) = (
                be32(&bytes, i * 4),
                be32(&bytes, 0x48 + i * 4),
                be32(&bytes, 0x90 + i * 4),
            );
            if size == 0 {
                continue;
            }
            let end = (offset as usize).checked_add(size as usize);
            if end.is_none_or(|e| e > bytes.len()) {
                return Err(DolError::BadSection(i));
            }
            let kind = if i < 7 {
                SectionKind::Text
            } else {
                SectionKind::Data
            };
            sections.push(Section {
                kind,
                addr,
                size,
                offset,
            });
        }
        Ok(Dol {
            entry: be32(&bytes, 0xE0),
            bss_addr: be32(&bytes, 0xD8),
            bss_size: be32(&bytes, 0xDC),
            sections,
            bytes,
        })
    }

    pub fn section_bytes(&self, s: &Section) -> &[u8] {
        &self.bytes[s.offset as usize..(s.offset + s.size) as usize]
    }

    pub fn text_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|s| s.kind == SectionKind::Text)
    }

    pub fn data_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|s| s.kind == SectionKind::Data)
    }

    /// Read a big-endian word from the initialised image (text or data).
    pub fn read_u32(&self, addr: u32) -> Option<u32> {
        let s = self
            .sections
            .iter()
            .find(|s| addr >= s.addr && addr - s.addr + 4 <= s.size)?;
        Some(be32(self.section_bytes(s), (addr - s.addr) as usize))
    }
}
