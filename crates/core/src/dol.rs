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
    Some(DolInfo { entry: be32(header, 0xE0), size })
}
