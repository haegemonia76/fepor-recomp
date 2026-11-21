//! The environment the IPL and apploader leave behind: DOL loaded, low-memory globals set.

use fepor_core::dol::Dol;
use fepor_core::Disc;

use crate::machine::{Machine, RAM_SIZE};

const TOP: u32 = 0x8000_0000 + RAM_SIZE as u32;
const BI2_SIZE: u32 = 0x2000;

const BUS_CLOCK: u32 = 162_000_000;
const CPU_CLOCK: u32 = 486_000_000;

/// TV format in 0x800000CC: 0 NTSC, 1 PAL, 5 EURGB60.
fn video_mode(game_id: &str) -> u32 {
    match game_id.as_bytes().get(3) {
        Some(b'P' | b'D' | b'F' | b'S' | b'I' | b'H' | b'U') => 1,
        _ => 0,
    }
}

/// Copy the DOL sections into RAM and fill the low-memory globals the SDK reads. Returns the entry point.
pub fn setup(m: &mut Machine, disc: &mut dyn Disc, dol: &Dol) -> Result<u32, String> {
    for s in &dol.sections {
        m.write_bytes(s.addr, dol.section_bytes(s));
    }

    let sys = disc.sys_files().map_err(|e| e.to_string())?;
    let get = |name: &str| sys.iter().find(|(n, _)| *n == name).map(|(_, d)| d.as_slice()).ok_or(format!("missing {name}"));
    let (boot, bi2, fst) = (get("boot.bin")?, get("bi2.bin")?, get("fst.bin")?);

    // The first 0x20 bytes of the disc header: game ID, maker, disc number, version, magic.
    m.write_bytes(0x8000_0000, &boot[..0x20]);
    // Disc information block mirrored where the DVD driver looks for it.
    m.write_bytes(0x8000_3180, &boot[..4]);

    // FST and BI2 live at the top of RAM.
    let fst_size = (fst.len() as u32 + 31) & !31;
    let fst_addr = TOP - fst_size;
    let bi2_addr = fst_addr - BI2_SIZE;
    m.write_bytes(fst_addr, fst);
    m.write_bytes(bi2_addr, &bi2[..bi2.len().min(BI2_SIZE as usize)]);

    m.write_u32(0x8000_0020, 0x0D15_EA5E); // boot kind: normal
    m.write_u32(0x8000_0024, 1); // version
    m.write_u32(0x8000_0028, RAM_SIZE as u32); // physical memory size
    m.write_u32(0x8000_002C, 1); // console type: retail
    m.write_u32(0x8000_0030, 0); // arena low (set by OSInit from the DOL end)
    m.write_u32(0x8000_0034, bi2_addr); // arena high
    m.write_u32(0x8000_0038, fst_addr);
    m.write_u32(0x8000_003C, fst_size);
    m.write_u32(0x8000_00CC, video_mode(disc.game_id()));
    m.write_u32(0x8000_00D0, 0x0100_0000); // ARAM size
    m.write_u32(0x8000_00EC, TOP); // top of RAM
    m.write_u32(0x8000_00F0, RAM_SIZE as u32); // simulated memory size
    m.write_u32(0x8000_00F4, bi2_addr);
    m.write_u32(0x8000_00F8, BUS_CLOCK);
    m.write_u32(0x8000_00FC, CPU_CLOCK);
    Ok(dol.entry)
}

/// Hand the disc to the DVD interface once boot data has been read from it.
pub fn attach_disc(m: &mut Machine, disc: Box<dyn Disc>) {
    m.hw.attach_disc(disc);
}
