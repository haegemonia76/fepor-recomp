//! Inspection of raw GameCube memory card images (Dolphin's `.raw` format): header, directory and
//! block-allocation-table checksums, and the saves stored on the card. Used to check that images the
//! recompiled game writes are structurally valid, i.e. loadable by Dolphin and by a real console.

const BLOCK: usize = 0x2000;
const DIR_ENTRIES: usize = 127;
const DIR_ENTRY: usize = 0x40;

#[derive(Debug, PartialEq, Eq)]
pub struct Save {
    pub game_code: String,
    pub maker_code: String,
    pub file_name: String,
    pub first_block: u16,
    pub blocks: u16,
}

#[derive(Debug)]
pub struct CardInfo {
    pub size_mbit: u32,
    pub formatted: bool,
    /// Which of the two header / directory / BAT copies are valid, and which is current.
    pub header_ok: bool,
    pub dir_ok: [bool; 2],
    pub bat_ok: [bool; 2],
    pub free_blocks: u16,
    pub saves: Vec<Save>,
}

fn be16(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}

/// The card's two-halfword checksum (sum and sum of complements) over big-endian halfwords.
pub fn checksum(data: &[u8]) -> (u16, u16) {
    let (mut c, mut ci) = (0u16, 0u16);
    for w in data.chunks_exact(2) {
        let v = u16::from_be_bytes([w[0], w[1]]);
        c = c.wrapping_add(v);
        ci = ci.wrapping_add(v ^ 0xFFFF);
    }
    (
        if c == 0xFFFF { 0 } else { c },
        if ci == 0xFFFF { 0 } else { ci },
    )
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]).into_owned()
}

pub fn inspect(image: &[u8]) -> Result<CardInfo, String> {
    let mbit_bytes = 1024 * 8 * 16;
    if image.len() % mbit_bytes != 0 || image.len() < 4 * mbit_bytes {
        return Err(format!(
            "{} bytes is not a memory card image size",
            image.len()
        ));
    }
    let size_mbit = (image.len() / mbit_bytes) as u32;
    let blank = image[..BLOCK * 5].iter().all(|&b| b == 0xFF);
    let header = &image[..BLOCK];
    let header_ok = checksum(&header[..0x1FC]) == (be16(header, 0x1FC), be16(header, 0x1FE));

    let dir_at = |i: usize| &image[BLOCK * (1 + i)..BLOCK * (2 + i)];
    let bat_at = |i: usize| &image[BLOCK * (3 + i)..BLOCK * (4 + i)];
    let dir_ok = [0, 1].map(|i| {
        let d = dir_at(i);
        checksum(&d[..0x1FFC]) == (be16(d, 0x1FFC), be16(d, 0x1FFE))
    });
    let bat_ok = [0, 1].map(|i| {
        let b = bat_at(i);
        checksum(&b[4..]) == (be16(b, 0), be16(b, 2))
    });

    // the copy with the higher update counter (and a valid checksum) is current
    let dir_counter = |i: usize| be16(dir_at(i), 0x1FFA);
    let cur_dir = match (dir_ok[0], dir_ok[1]) {
        (true, true) => usize::from(
            dir_counter(1).wrapping_sub(dir_counter(0)) < 0x8000
                && dir_counter(1) != dir_counter(0),
        ),
        (false, true) => 1,
        _ => 0,
    };
    let bat_counter = |i: usize| be16(bat_at(i), 4);
    let cur_bat = match (bat_ok[0], bat_ok[1]) {
        (true, true) => usize::from(
            bat_counter(1).wrapping_sub(bat_counter(0)) < 0x8000
                && bat_counter(1) != bat_counter(0),
        ),
        (false, true) => 1,
        _ => 0,
    };

    let mut saves = Vec::new();
    if !blank {
        let d = dir_at(cur_dir);
        for e in 0..DIR_ENTRIES {
            let ent = &d[e * DIR_ENTRY..(e + 1) * DIR_ENTRY];
            if ent[..4] == [0xFF; 4] {
                continue;
            }
            saves.push(Save {
                game_code: text(&ent[0..4]),
                maker_code: text(&ent[4..6]),
                file_name: text(&ent[8..40]),
                first_block: be16(ent, 0x36),
                blocks: be16(ent, 0x38),
            });
        }
    }
    Ok(CardInfo {
        size_mbit,
        formatted: !blank && header_ok,
        header_ok,
        dir_ok,
        bat_ok,
        free_blocks: be16(bat_at(cur_bat), 6),
        saves,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_checksum(buf: &mut [u8], data_end: usize, at: usize) {
        let (c, ci) = checksum(&buf[..data_end]);
        buf[at..at + 2].copy_from_slice(&c.to_be_bytes());
        buf[at + 2..at + 4].copy_from_slice(&ci.to_be_bytes());
    }

    /// A minimal formatted 4 Mbit card with one save.
    fn sample() -> Vec<u8> {
        let mut img = vec![0xFFu8; 4 * 1024 * 8 * 16];
        img[..BLOCK].fill(0);
        img[0x14..0x16].copy_from_slice(&4u16.to_be_bytes());
        put_checksum(&mut img[..BLOCK], 0x1FC, 0x1FC);
        for i in 0..2 {
            let d = &mut img[BLOCK * (1 + i)..BLOCK * (2 + i)];
            d.fill(0xFF);
            d[0x1FC0..0x1FFC].fill(0);
            d[0x1FFA..0x1FFC].copy_from_slice(&(i as u16).to_be_bytes());
            if i == 1 {
                d[..4].copy_from_slice(b"GFEP");
                d[4..6].copy_from_slice(b"01");
                d[8..8 + 8].copy_from_slice(b"FE-PoR\0\0");
                d[0x36..0x38].copy_from_slice(&5u16.to_be_bytes());
                d[0x38..0x3A].copy_from_slice(&3u16.to_be_bytes());
            }
            put_checksum(d, 0x1FFC, 0x1FFC);
            let b = &mut img[BLOCK * (3 + i)..BLOCK * (4 + i)];
            b.fill(0);
            b[4..6].copy_from_slice(&(i as u16).to_be_bytes());
            b[6..8].copy_from_slice(&(56u16).to_be_bytes());
            let (c, ci) = checksum(&b[4..]);
            b[0..2].copy_from_slice(&c.to_be_bytes());
            b[2..4].copy_from_slice(&ci.to_be_bytes());
        }
        img
    }

    #[test]
    fn blank_card_is_unformatted() {
        let info = inspect(&vec![0xFF; 4 * 1024 * 8 * 16]).unwrap();
        assert!(!info.formatted && info.saves.is_empty());
    }

    #[test]
    fn formatted_card_lists_the_current_directory() {
        let info = inspect(&sample()).unwrap();
        assert!(
            info.formatted
                && info.header_ok
                && info.dir_ok == [true, true]
                && info.bat_ok == [true, true]
        );
        assert_eq!(info.free_blocks, 56);
        assert_eq!(
            info.saves,
            vec![Save {
                game_code: "GFEP".into(),
                maker_code: "01".into(),
                file_name: "FE-PoR".into(),
                first_block: 5,
                blocks: 3
            }]
        );
    }

    #[test]
    fn corrupted_header_is_reported() {
        let mut img = sample();
        img[0x20] ^= 1;
        assert!(!inspect(&img).unwrap().header_ok);
    }
}
