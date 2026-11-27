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

fn put16(d: &mut [u8], o: usize, v: u16) {
    d[o..o + 2].copy_from_slice(&v.to_be_bytes());
}

fn fix_checksum(block: &mut [u8], range_end: usize, at: usize, from: usize) {
    let (c, ci) = checksum(&block[from..range_end]);
    put16(block, at, c);
    put16(block, at + 2, ci);
}

/// Index (0 or 1) of the current copy of the directory and of the BAT, and whether the image is formatted.
fn current_copies(image: &[u8]) -> Result<(usize, usize), String> {
    let info = inspect(image)?;
    if !info.formatted {
        return Err("the card is not formatted".into());
    }
    let pick = |ok: [bool; 2], counter: &dyn Fn(usize) -> u16| match (ok[0], ok[1]) {
        (true, true) => {
            usize::from(counter(1).wrapping_sub(counter(0)) < 0x8000 && counter(1) != counter(0))
        }
        (false, true) => 1,
        _ => 0,
    };
    let dir = pick(info.dir_ok, &|i| be16(&image[BLOCK * (1 + i)..], 0x1FFA));
    let bat = pick(info.bat_ok, &|i| be16(&image[BLOCK * (3 + i)..], 4));
    Ok((dir, bat))
}

/// Add a save (a `.gci` file: the 0x40-byte directory entry followed by the file's blocks) to the card,
/// the way a console does it: the unused copy of the directory and BAT is updated and its counter raised.
pub fn import_gci(image: &mut [u8], gci: &[u8]) -> Result<(), String> {
    if gci.len() < DIR_ENTRY {
        return Err("not a GCI file".into());
    }
    let (entry, data) = gci.split_at(DIR_ENTRY);
    let blocks = be16(entry, 0x38) as usize;
    if blocks == 0 || data.len() < blocks * BLOCK {
        return Err(format!(
            "GCI declares {blocks} blocks but holds {} bytes of data",
            data.len()
        ));
    }
    let (cd, cb) = current_copies(image)?;
    let (nd, nb) = (1 - cd, 1 - cb);
    let total_blocks = image.len() / BLOCK;
    let mut dir = image[BLOCK * (1 + cd)..BLOCK * (2 + cd)].to_vec();
    let mut bat = image[BLOCK * (3 + cb)..BLOCK * (4 + cb)].to_vec();

    // name clash and free slot
    let mut slot = None;
    for e in 0..DIR_ENTRIES {
        let ent = &dir[e * DIR_ENTRY..(e + 1) * DIR_ENTRY];
        if ent[..4] == [0xFF; 4] {
            slot.get_or_insert(e);
        } else if ent[..6] == entry[..6] && ent[8..40] == entry[8..40] {
            return Err("the card already holds a save with this name".into());
        }
    }
    let slot = slot.ok_or("the card's directory is full")?;
    let free = be16(&bat, 6) as usize;
    if free < blocks {
        return Err(format!(
            "not enough free blocks ({free} free, {blocks} needed)"
        ));
    }

    // allocate from the block after the last allocated one, wrapping
    let mut chain = Vec::with_capacity(blocks);
    let mut b = be16(&bat, 8) as usize;
    for _ in 0..total_blocks {
        if chain.len() == blocks {
            break;
        }
        b += 1;
        if b >= total_blocks || b < 5 {
            b = 5;
        }
        if be16(&bat, 10 + 2 * (b - 5)) == 0 {
            chain.push(b);
        }
    }
    if chain.len() < blocks {
        return Err("the block allocation table has too few free blocks".into());
    }
    for (i, &blk) in chain.iter().enumerate() {
        let next = chain.get(i + 1).map_or(0xFFFF, |&n| n as u16);
        put16(&mut bat, 10 + 2 * (blk - 5), next);
        image[blk * BLOCK..(blk + 1) * BLOCK].copy_from_slice(&data[i * BLOCK..(i + 1) * BLOCK]);
    }
    put16(&mut bat, 6, (free - blocks) as u16);
    put16(&mut bat, 8, *chain.last().expect("blocks > 0") as u16);
    let bat_counter = be16(&bat, 4).wrapping_add(1);
    put16(&mut bat, 4, bat_counter);
    fix_checksum(&mut bat, BLOCK, 0, 4);

    let mut ent = entry.to_vec();
    put16(&mut ent, 0x36, chain[0] as u16);
    dir[slot * DIR_ENTRY..(slot + 1) * DIR_ENTRY].copy_from_slice(&ent);
    let dir_counter = be16(&dir, 0x1FFA).wrapping_add(1);
    put16(&mut dir, 0x1FFA, dir_counter);
    fix_checksum(&mut dir, 0x1FFC, 0x1FFC, 0);

    image[BLOCK * (1 + nd)..BLOCK * (2 + nd)].copy_from_slice(&dir);
    image[BLOCK * (3 + nb)..BLOCK * (4 + nb)].copy_from_slice(&bat);
    Ok(())
}

/// The saves on the card as `(file name, directory entry index)`.
pub fn list(image: &[u8]) -> Result<Vec<(String, usize)>, String> {
    let (cd, _) = current_copies(image)?;
    let dir = &image[BLOCK * (1 + cd)..BLOCK * (2 + cd)];
    Ok((0..DIR_ENTRIES)
        .filter(|&e| dir[e * DIR_ENTRY..][..4] != [0xFF; 4])
        .map(|e| (text(&dir[e * DIR_ENTRY + 8..e * DIR_ENTRY + 40]), e))
        .collect())
}

/// Take save number `entry` (see [`list`]) off the card as a `.gci` file.
pub fn export_gci(image: &[u8], entry: usize) -> Result<Vec<u8>, String> {
    let (cd, cb) = current_copies(image)?;
    let dir = &image[BLOCK * (1 + cd)..BLOCK * (2 + cd)];
    let bat = &image[BLOCK * (3 + cb)..BLOCK * (4 + cb)];
    let ent = dir
        .get(entry * DIR_ENTRY..(entry + 1) * DIR_ENTRY)
        .ok_or("no such directory entry")?;
    if ent[..4] == [0xFF; 4] {
        return Err("that directory entry is empty".into());
    }
    let (first, blocks) = (be16(ent, 0x36) as usize, be16(ent, 0x38) as usize);
    let mut out = ent.to_vec();
    let mut b = first;
    for _ in 0..blocks {
        if b < 5 || b >= image.len() / BLOCK {
            return Err(format!("broken block chain at block {b}"));
        }
        out.extend_from_slice(&image[b * BLOCK..(b + 1) * BLOCK]);
        b = be16(bat, 10 + 2 * (b - 5)) as usize;
    }
    Ok(out)
}

/// Backup copies of a card image, `<data>/memcards/backups/<card name>.<unix seconds>.raw`, newest kept.
pub fn backups_dir(data: &std::path::Path) -> std::path::PathBuf {
    data.join("memcards").join("backups")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    pub path: std::path::PathBuf,
    /// Seconds since the Unix epoch when the copy was made.
    pub stamp: u64,
}

fn card_stem(card: &std::path::Path) -> String {
    card.file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
}

/// The backups of `card` in `dir`, newest first.
pub fn list_backups(dir: &std::path::Path, card: &std::path::Path) -> Vec<Backup> {
    let prefix = format!("{}.", card_stem(card));
    let mut out: Vec<Backup> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_name()?.to_str()?;
            let stamp = name.strip_prefix(&prefix)?.strip_suffix(".raw")?;
            if stamp.is_empty() || !stamp.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let stamp = stamp.parse().ok()?;
            Some(Backup { path, stamp })
        })
        .collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.stamp));
    out
}

/// Copy `card` into `dir` unless the newest backup already holds the same bytes, then delete the oldest
/// copies beyond `keep`. `keep == 0` turns backups off. A missing card is not an error (nothing to save).
/// Returns the new copy.
pub fn backup(
    card: &std::path::Path,
    dir: &std::path::Path,
    keep: usize,
    now: u64,
) -> std::io::Result<Option<std::path::PathBuf>> {
    if keep == 0 {
        return Ok(None);
    }
    let image = match std::fs::read(card) {
        Ok(i) => i,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let existing = list_backups(dir, card);
    if let Some(newest) = existing.first() {
        if std::fs::read(&newest.path).is_ok_and(|b| b == image) {
            return Ok(None);
        }
    }
    std::fs::create_dir_all(dir)?;
    // a second copy within the same second replaces the first
    let stamp = existing.first().map_or(now, |n| now.max(n.stamp));
    let path = dir.join(format!("{}.{stamp:010}.raw", card_stem(card)));
    std::fs::write(&path, &image)?;
    for old in list_backups(dir, card).into_iter().skip(keep) {
        let _ = std::fs::remove_file(old.path);
    }
    Ok(Some(path))
}

/// Replace `card` by the backup `from` (a card image of a valid size).
pub fn restore(card: &std::path::Path, from: &std::path::Path) -> std::io::Result<()> {
    let image = std::fs::read(from)?;
    if image.is_empty() || !image.len().is_multiple_of(BLOCK) {
        return Err(std::io::Error::other("not a memory card image"));
    }
    std::fs::write(card, image)
}

const PENDING_RESTORE: &str = "restore.pending";

/// Ask for the backup file `name` of `dir` to replace the card at the next start (the running game holds the
/// card open and would overwrite a restore).
pub fn queue_restore(dir: &std::path::Path, name: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(PENDING_RESTORE), name)
}

/// The queued restore, consumed. Only names of files in `dir` are accepted.
pub fn take_restore(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let marker = dir.join(PENDING_RESTORE);
    let name = std::fs::read_to_string(&marker).ok()?;
    let _ = std::fs::remove_file(&marker);
    let name = name.trim();
    let path = dir.join(name);
    (std::path::Path::new(name)
        .file_name()
        .is_some_and(|n| n == name)
        && path.is_file())
    .then_some(path)
}

/// `YYYY-MM-DD HH:MM:SS` (UTC) of a Unix time.
pub fn format_stamp(secs: u64) -> String {
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
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

    #[test]
    fn import_then_export_round_trips_and_checksums_stay_valid() {
        let mut img = sample();
        let mut gci = vec![0u8; DIR_ENTRY + 2 * BLOCK];
        gci[..4].copy_from_slice(b"GFEP");
        gci[4..6].copy_from_slice(b"01");
        gci[8..8 + 5].copy_from_slice(b"SAVE2");
        gci[0x38..0x3A].copy_from_slice(&2u16.to_be_bytes());
        for (i, b) in gci[DIR_ENTRY..].iter_mut().enumerate() {
            *b = (i * 7) as u8;
        }
        import_gci(&mut img, &gci).unwrap();
        let info = inspect(&img).unwrap();
        assert!(info.formatted && info.dir_ok == [true, true] && info.bat_ok == [true, true]);
        assert_eq!(info.free_blocks, 54);
        assert_eq!(info.saves.len(), 2);
        let names = list(&img).unwrap();
        let (_, idx) = names.iter().find(|(n, _)| n == "SAVE2").unwrap();
        let out = export_gci(&img, *idx).unwrap();
        assert_eq!(&out[DIR_ENTRY..], &gci[DIR_ENTRY..]);
        // importing the same name twice is refused
        assert!(import_gci(&mut img, &gci).is_err());
    }

    #[test]
    fn backups_rotate_and_skip_unchanged_images() {
        let t = tempfile::tempdir().unwrap();
        let card = t.path().join("MemoryCardA.USA.raw");
        let dir = t.path().join("backups");
        assert_eq!(
            backup(&card, &dir, 3, 100).unwrap(),
            None,
            "no card, no backup"
        );
        for (i, now) in [100u64, 200, 300, 400].into_iter().enumerate() {
            std::fs::write(&card, vec![i as u8; BLOCK]).unwrap();
            assert!(backup(&card, &dir, 3, now).unwrap().is_some());
            assert_eq!(backup(&card, &dir, 3, now + 1).unwrap(), None, "unchanged");
        }
        let stamps: Vec<u64> = list_backups(&dir, &card).iter().map(|b| b.stamp).collect();
        assert_eq!(stamps, [400, 300, 200]);
        // other cards' backups are not listed nor rotated
        std::fs::write(dir.join("MemoryCardB.USA.0000000001.raw"), [0]).unwrap();
        assert_eq!(list_backups(&dir, &card).len(), 3);
        assert_eq!(backup(&card, &dir, 0, 500).unwrap(), None, "keep 0 is off");
    }

    #[test]
    fn restore_replaces_the_card_through_a_queued_request() {
        let t = tempfile::tempdir().unwrap();
        let card = t.path().join("MemoryCardA.USA.raw");
        let dir = t.path().join("backups");
        std::fs::write(&card, vec![1u8; BLOCK]).unwrap();
        let first = backup(&card, &dir, 5, 10).unwrap().unwrap();
        std::fs::write(&card, vec![2u8; BLOCK]).unwrap();
        queue_restore(&dir, first.file_name().unwrap().to_str().unwrap()).unwrap();
        let from = take_restore(&dir).unwrap();
        restore(&card, &from).unwrap();
        assert_eq!(std::fs::read(&card).unwrap(), vec![1u8; BLOCK]);
        assert_eq!(take_restore(&dir), None, "consumed");
        queue_restore(&dir, "../MemoryCardA.USA.raw").unwrap();
        assert_eq!(take_restore(&dir), None, "only files of the backup folder");
        std::fs::write(&from, [1, 2, 3]).unwrap();
        assert!(restore(&card, &from).is_err(), "not a card image");
    }

    #[test]
    fn stamps_read_as_utc_dates() {
        assert_eq!(format_stamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_stamp(1_796_000_000), "2026-11-30 00:53:20 UTC");
    }
}
