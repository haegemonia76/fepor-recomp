//! Builds a tiny synthetic GCM image, then checks ISO reading, extraction and folder re-reading.

use fepor_core::{dol, extract::extract, open_disc, version, Config};

fn put32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

fn synthetic_iso() -> (Vec<u8>, Vec<u8>) {
    const DOL_OFF: usize = 0x20000;
    const FST_OFF: usize = 0x21000;
    const FILE_A: usize = 0x22000;
    const FILE_B: usize = 0x23000;

    let mut img = vec![0u8; 0x24000];
    img[..6].copy_from_slice(b"GTEST1");
    put32(&mut img, 0x1C, 0xC233_9F3D);
    put32(&mut img, 0x420, DOL_OFF as u32);
    // apploader header: size 0x10, trailer 0
    put32(&mut img, 0x2440 + 0x14, 0x10);

    // DOL: one text section of 0x40 bytes at file offset 0x100
    let mut dol_bytes = vec![0u8; 0x140];
    put32(&mut dol_bytes, 0, 0x100);
    put32(&mut dol_bytes, 0x48, 0x8000_3100);
    put32(&mut dol_bytes, 0x90, 0x40);
    put32(&mut dol_bytes, 0xE0, 0x8000_3154);
    dol_bytes[0x100..0x140].fill(0xAB);
    img[DOL_OFF..DOL_OFF + 0x140].copy_from_slice(&dol_bytes);

    // FST: root, dir "d" containing "b.bin", then root-level "a.txt" (exercises dir exit)
    let names = b"d\0b.bin\0a.txt\0";
    let mut fst = vec![0u8; 4 * 12];
    fst[0] = 1;
    put32(&mut fst, 8, 4);
    fst[12] = 1; // dir "d", name offset 0, parent 0, end index 3
    put32(&mut fst, 12 + 8, 3);
    fst[24 + 3] = 2; // file "b.bin"
    put32(&mut fst, 24 + 4, FILE_B as u32);
    put32(&mut fst, 24 + 8, 5);
    fst[36 + 3] = 8; // file "a.txt"
    put32(&mut fst, 36 + 4, FILE_A as u32);
    put32(&mut fst, 36 + 8, 3);
    fst.extend_from_slice(names);
    put32(&mut img, 0x424, FST_OFF as u32);
    put32(&mut img, 0x428, fst.len() as u32);
    img[FST_OFF..FST_OFF + fst.len()].copy_from_slice(&fst);
    img[FILE_A..FILE_A + 3].copy_from_slice(b"abc");
    img[FILE_B..FILE_B + 5].copy_from_slice(b"hello");
    (img, dol_bytes)
}

#[test]
fn iso_extract_then_folder_read_match() {
    let tmp = tempfile::tempdir().unwrap();
    let (img, dol_bytes) = synthetic_iso();
    let iso = tmp.path().join("t.iso");
    std::fs::write(&iso, &img).unwrap();

    let mut d = open_disc(&iso).unwrap();
    assert_eq!(d.game_id(), "GTEST1");
    let paths: Vec<_> = d.files().iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["d/b.bin", "a.txt"]);
    assert_eq!(d.dol().unwrap(), dol_bytes);
    assert_eq!(dol::parse_header(&dol_bytes).unwrap().entry, 0x8000_3154);

    let out = tmp.path().join("out");
    let report = extract(&mut *d, &out, |_| {}).unwrap();
    assert_eq!((report.sys_files, report.files), (5, 2));
    assert_eq!(std::fs::read(out.join("files/d/b.bin")).unwrap(), b"hello");
    assert_eq!(std::fs::read(out.join("files/a.txt")).unwrap(), b"abc");
    assert_eq!(std::fs::read(out.join("sys/main.dol")).unwrap(), dol_bytes);

    let mut f = open_disc(&out).unwrap();
    assert_eq!(f.game_id(), "GTEST1");
    assert_eq!(f.dol().unwrap(), dol_bytes);
    let mut paths: Vec<_> = f.files().iter().map(|f| f.path.clone()).collect();
    paths.sort();
    assert_eq!(paths, ["a.txt", "d/b.bin"]);
}

#[test]
fn rejects_non_gcm_and_unknown_version() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("bad.iso");
    std::fs::write(&bad, vec![0u8; 0x1000]).unwrap();
    assert!(open_disc(&bad).is_err());

    assert_eq!(
        version::check("GXXX01", b"").unwrap_err(),
        version::VersionError::Unsupported("GXXX01".into())
    );
    assert!(matches!(
        version::check("GFEP01", b"not the dol"),
        Err(version::VersionError::HashMismatch { .. })
    ));
}

#[test]
fn config_roundtrip_and_unknown_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("c.toml");
    let c = Config {
        disc: Some("a.iso".into()),
        output: Some("out".into()),
        ..Config::default()
    };
    c.save(&p).unwrap();
    assert_eq!(Config::load(&p).unwrap(), c);
    std::fs::write(&p, "bogus = 1").unwrap();
    assert!(Config::load(&p).is_err());
}

#[test]
fn analysis_config_parses_hex_and_merges_builtin_defaults() {
    let user: Config = toml::from_str(
        "[analysis]\nfunctions = [0x80003154]\n[[analysis.exclude]]\nstart = 0x80003100\nend = 0x80003110\n[[analysis.jump_tables]]\nbranch = 0x8023528c\ntargets = [0x80235290]\n",
    )
    .unwrap();
    assert_eq!(user.analysis.functions, [0x8000_3154]);
    let defaults = version::lookup("GFEP01").unwrap().analysis_defaults();
    assert!(!defaults.exclude.is_empty());
    let merged = user.analysis.with_defaults(defaults.clone());
    assert_eq!(merged.exclude.len(), 1 + defaults.exclude.len());
    assert_eq!(merged.exclude[0].start, 0x8000_3100);
    assert_eq!(merged.jump_tables[0].targets, [0x8023_5290]);
}
