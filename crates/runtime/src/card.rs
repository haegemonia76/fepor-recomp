//! GameCube memory card on an EXI channel: the card's command protocol over a raw image file.
//!
//! The image is Dolphin's raw format (`MemoryCardA.<region>.raw`): the card contents byte for byte, no
//! header, `size = megabits * 131072` (4 Mbit = "Memory Card 59", 16 Mbit = "251"). The SDK's CARD
//! library (guest code) talks to this device exactly as it would to the hardware, so formatting,
//! directory and BAT handling are the game's own. The command set and status behaviour follow Dolphin's
//! EXI memory card device (GPL-2.0-or-later) so saves made here load in Dolphin and the other way round.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const STATUS_BUSY: u8 = 0x80;
const STATUS_UNLOCKED: u8 = 0x40;
const STATUS_ERASE_ERROR: u8 = 0x10;
const STATUS_PROGRAM_ERROR: u8 = 0x08;
const STATUS_READY: u8 = 0x01;

/// Bytes per megabit of card capacity.
const MBIT: usize = 1024 * 8 * 16;
/// Erase unit (one card block).
const BLOCK: usize = 0x2000;

mod cmd {
    pub const NINTENDO_ID: u8 = 0x00;
    pub const READ_ARRAY: u8 = 0x52;
    pub const SET_INTERRUPT: u8 = 0x81;
    pub const READ_STATUS: u8 = 0x83;
    pub const READ_ID: u8 = 0x85;
    pub const CLEAR_STATUS: u8 = 0x89;
    pub const SECTOR_ERASE: u8 = 0xF1;
    pub const PAGE_PROGRAM: u8 = 0xF2;
    pub const CHIP_ERASE: u8 = 0xF4;
}

pub struct MemCard {
    file: File,
    data: Vec<u8>,
    status: u8,
    interrupt_switch: u8,
    interrupt_set: bool,
    command: u8,
    position: u32,
    address: u32,
    buffer: [u8; 128],
}

impl MemCard {
    /// Open `path`, creating an unformatted (all 0xFF) card of `default_mbit` megabits when it does not exist.
    /// The game formats it, as it would a fresh card.
    pub fn open(path: &Path, default_mbit: usize) -> io::Result<MemCard> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        if data.is_empty() {
            data = vec![0xFF; default_mbit * MBIT];
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&data)?;
        } else if data.len() % MBIT != 0 || !(4..=128).contains(&(data.len() / MBIT)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: not a raw memory card image ({} bytes)",
                    path.display(),
                    data.len()
                ),
            ));
        }
        Ok(MemCard {
            file,
            data,
            status: STATUS_BUSY | STATUS_UNLOCKED | STATUS_READY,
            interrupt_switch: 0,
            interrupt_set: false,
            command: cmd::NINTENDO_ID,
            position: 0,
            address: 0,
            buffer: [0; 128],
        })
    }

    /// The card size in megabits, which is also its EXI ID (4 = 59 blocks, 16 = 251 blocks).
    pub fn card_id(&self) -> u32 {
        (self.data.len() / MBIT) as u32
    }

    /// The first 20 header bytes (serial number etc.), used to derive the flash ID the OS keeps in SRAM.
    pub fn header(&self) -> [u8; 20] {
        let mut h = [0u8; 20];
        h.copy_from_slice(&self.data[..20]);
        h
    }

    pub fn interrupt_pending(&self) -> bool {
        self.interrupt_switch != 0 && self.interrupt_set
    }

    fn mask(&self) -> usize {
        self.data.len() - 1
    }

    fn persist(&mut self, from: usize, len: usize) {
        if self.file.seek(SeekFrom::Start(from as u64)).is_ok() {
            let _ = self.file.write_all(&self.data[from..from + len]);
        }
    }

    fn read_at(&self, addr: usize, out: &mut [u8]) {
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.data[(addr + i) & self.mask()];
        }
    }

    fn write_at(&mut self, addr: usize, bytes: &[u8]) {
        // programming can only clear bits
        let n = bytes.len().min(self.data.len());
        for (i, b) in bytes.iter().take(n).enumerate() {
            let a = (addr + i) & self.mask();
            self.data[a] &= *b;
        }
        let start = addr & self.mask();
        let len = n.min(self.data.len() - start);
        self.persist(start, len);
    }

    fn clear_block(&mut self, addr: usize) {
        let start = addr & !(BLOCK - 1) & self.mask();
        self.data[start..start + BLOCK].fill(0xFF);
        self.persist(start, BLOCK);
    }

    fn command_done(&mut self) {
        self.status |= STATUS_READY;
        self.status &= !STATUS_BUSY;
        self.interrupt_set = true;
    }

    /// Chip select changed.
    pub fn set_cs(&mut self, selected: bool) {
        if selected {
            self.position = 0;
            return;
        }
        match self.command {
            cmd::SECTOR_ERASE if self.position > 2 => {
                let a = self.address as usize & self.mask();
                self.clear_block(a);
                self.command_done();
            }
            cmd::CHIP_ERASE if self.position > 2 => {
                self.data.fill(0xFF);
                self.persist(0, self.data.len());
                self.status &= !STATUS_BUSY;
            }
            cmd::PAGE_PROGRAM if self.position >= 5 => {
                // bytes clocked in one at a time (the SDK normally uses DMA, which writes directly)
                let count = self.position as usize - 5;
                let mut addr = self.address as usize;
                for i in 0..count {
                    let b = self.buffer[i & 127];
                    self.write_at(addr, &[b]);
                    addr = (addr & !0x1FF) | ((addr + 1) & 0x1FF);
                }
                self.address = addr as u32;
                self.command_done();
            }
            _ => {}
        }
    }

    /// One byte of the command stream; returns the byte the card sends back.
    pub fn transfer_byte(&mut self, mut byte: u8) -> u8 {
        if self.position == 0 {
            self.command = byte;
            byte = 0xFF;
            if self.command == cmd::CLEAR_STATUS {
                self.status &= !(STATUS_PROGRAM_ERROR | STATUS_ERASE_ERROR);
                self.status |= STATUS_READY;
                self.interrupt_set = false;
                self.position = 0;
                return 0xFF;
            }
        } else {
            match self.command {
                cmd::NINTENDO_ID => {
                    byte = if self.position == 1 {
                        0x80
                    } else {
                        (self.card_id() >> (24 - (((self.position - 2) & 3) * 8))) as u8
                    };
                }
                cmd::READ_ARRAY => {
                    match self.position {
                        1 => {
                            self.address = (byte as u32) << 17;
                            byte = 0xFF;
                        }
                        2 => self.address |= (byte as u32) << 9,
                        3 => self.address |= ((byte & 3) as u32) << 7,
                        4 => self.address |= (byte & 0x7F) as u32,
                        _ => {}
                    }
                    if self.position > 1 {
                        let mut b = [0u8];
                        self.read_at(self.address as usize, &mut b);
                        byte = b[0];
                        if self.position >= 9 {
                            self.address = (self.address & !0x1FF) | ((self.address + 1) & 0x1FF);
                        }
                    }
                }
                cmd::READ_STATUS => byte = self.status,
                cmd::READ_ID => {
                    let id = 0xC221u16;
                    byte = if self.position == 1 {
                        (id >> 8) as u8
                    } else if self.position & 1 != 0 {
                        id as u8
                    } else {
                        (id >> 8) as u8
                    };
                }
                cmd::SECTOR_ERASE => {
                    match self.position {
                        1 => self.address = (byte as u32) << 17,
                        2 => self.address |= (byte as u32) << 9,
                        _ => {}
                    }
                    byte = 0xFF;
                }
                cmd::SET_INTERRUPT => {
                    if self.position == 1 {
                        self.interrupt_switch = byte;
                    }
                    byte = 0xFF;
                }
                cmd::CHIP_ERASE => byte = 0xFF,
                cmd::PAGE_PROGRAM => {
                    match self.position {
                        1 => self.address = (byte as u32) << 17,
                        2 => self.address |= (byte as u32) << 9,
                        3 => self.address |= ((byte & 3) as u32) << 7,
                        4 => self.address |= (byte & 0x7F) as u32,
                        _ => {}
                    }
                    if self.position >= 5 {
                        self.buffer[(self.position as usize - 5) & 0x7F] = byte;
                    }
                    byte = 0xFF;
                }
                _ => byte = 0xFF,
            }
        }
        self.position += 1;
        byte
    }

    /// DMA from the card (after the command bytes set the address).
    pub fn dma_read(&mut self, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.read_at(self.address as usize, &mut out);
        out
    }

    /// DMA to the card.
    pub fn dma_write(&mut self, bytes: &[u8]) {
        let a = self.address as usize;
        self.write_at(a, bytes);
    }
}

impl MemCard {
    /// The card as a save state keeps it: the whole image and the command state machine.
    pub fn save(&self, w: &mut fepor_core::snap::W) {
        w.bytes(&self.data);
        w.u8(self.status);
        w.u8(self.interrupt_switch);
        w.bool(self.interrupt_set);
        w.u8(self.command);
        w.u32(self.position);
        w.u32(self.address);
        w.buf.extend_from_slice(&self.buffer);
    }

    /// Restore it and write the image back to the file when it differs: the saves on the card are part
    /// of the machine's state (the game keeps a copy of the directory in RAM), so they go back with it.
    pub fn load(&mut self, r: &mut fepor_core::snap::R) {
        let data = r.bytes();
        if data.len() != self.data.len() {
            r.failed = true;
            return;
        }
        if data != &self.data[..] {
            self.data.copy_from_slice(data);
            self.persist(0, self.data.len());
        }
        self.status = r.u8();
        self.interrupt_switch = r.u8();
        self.interrupt_set = r.bool();
        self.command = r.u8();
        self.position = r.u32();
        self.address = r.u32();
        for b in self.buffer.iter_mut() {
            *b = r.u8();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, mbit: usize) -> (MemCard, std::path::PathBuf) {
        let p = std::env::temp_dir().join(format!("fepor-card-{}-{name}.raw", std::process::id()));
        let _ = std::fs::remove_file(&p);
        (MemCard::open(&p, mbit).unwrap(), p)
    }

    fn send(c: &mut MemCard, bytes: &[u8]) -> Vec<u8> {
        c.set_cs(true);
        bytes.iter().map(|&b| c.transfer_byte(b)).collect()
    }

    #[test]
    fn id_and_size_follow_the_image() {
        let (mut c, p) = card("id", 16);
        assert_eq!(c.card_id(), 16);
        let r = send(&mut c, &[0x00, 0, 0, 0, 0, 0, 0]);
        // dummy cycle then the ID big-endian: 0x00000010
        assert_eq!(&r[2..6], &[0, 0, 0, 0x10]);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn erase_program_and_read_back_persist_in_the_file() {
        let (mut c, p) = card("rw", 4);
        // sector erase of block 1 (address 0x2000 = AD1 0x00, AD2 0x10)
        send(&mut c, &[0xF1, 0x00, 0x10]);
        c.set_cs(false);
        // page program 4 bytes at 0x2000
        send(
            &mut c,
            &[0xF2, 0x00, 0x10, 0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF],
        );
        c.set_cs(false);
        // read array at 0x2000: command, 4 address bytes, 4 dummy bytes, then data
        let r = send(
            &mut c,
            &[0x52, 0x00, 0x10, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        assert_eq!(&r[9..13], &[0xDE, 0xAD, 0xBE, 0xEF]);
        // bits can only be cleared by programming; the status reports ready with the interrupt armed after commands
        drop(c);
        let on_disk = std::fs::read(&p).unwrap();
        assert_eq!(&on_disk[0x2000..0x2004], &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(on_disk.len(), 4 * MBIT);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn dma_write_then_read_round_trips() {
        let (mut c, p) = card("dma", 4);
        send(&mut c, &[0xF1, 0x00, 0x20]);
        c.set_cs(false);
        send(&mut c, &[0xF2, 0x00, 0x20, 0x00, 0x00]);
        c.dma_write(&[1, 2, 3, 4, 5, 6, 7, 8]);
        c.set_cs(false);
        send(&mut c, &[0x52, 0x00, 0x20, 0x00, 0x00]);
        assert_eq!(c.dma_read(8), vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let _ = std::fs::remove_file(p);
    }
}
