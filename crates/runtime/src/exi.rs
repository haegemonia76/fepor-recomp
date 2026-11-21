//! External Interface (EXI): channel registers plus the devices the SDK talks to during boot
//! (IPL ROM with RTC and SRAM, empty memory card slots).
//!
//! Transfers complete immediately: writing CR with TSTART performs the transfer and clears TSTART.

const STRIDE: u32 = 0x14;
const CSR: u32 = 0x00;
const MAR: u32 = 0x04;
const LENGTH: u32 = 0x08;
const CR: u32 = 0x0C;
const DATA: u32 = 0x10;

const CSR_EXIINT: u32 = 1 << 1;
const CSR_TCINT: u32 = 1 << 3;
const CSR_EXTINT: u32 = 1 << 11;
const CSR_EXT: u32 = 1 << 12;
/// Interrupt flags are cleared by writing 1.
const CSR_W1C: u32 = CSR_EXIINT | CSR_TCINT | CSR_EXTINT;

const RTC_CMD: u32 = 0x2000_0000;
const SRAM_CMD: u32 = 0x2000_0100;
const CMD_WRITE: u32 = 0x8000_0000;

#[derive(Default, Clone)]
struct Channel {
    csr: u32,
    mar: u32,
    length: u32,
    cr: u32,
    data: u32,
    /// Command word of the transfer sequence in progress on the selected device.
    cmd: Option<u32>,
    /// Bytes of the current command already transferred.
    pos: u32,
}

pub struct Exi {
    ch: [Channel; 3],
    sram: [u8; 64],
    /// Seconds since 2000-01-01 the RTC reports.
    rtc: u32,
}

/// Default SRAM with valid checksums, English.
fn default_sram() -> [u8; 64] {
    let mut s = [0u8; 64];
    s[0x13] = 0x01; // flags/language byte
    // checksum: sum of the big-endian u16 words in bytes 0x0C..0x14, then its complement
    let sum: u32 = (0x0C..0x14).step_by(2).map(|i| u16::from_be_bytes([s[i], s[i + 1]]) as u32).sum();
    let c = (sum & 0xFFFF) as u16;
    s[0..2].copy_from_slice(&c.to_be_bytes());
    s[2..4].copy_from_slice(&(!c).to_be_bytes());
    s
}

impl Exi {
    pub fn new() -> Exi {
        Exi { ch: Default::default(), sram: default_sram(), rtc: 0x2C00_0000 }
    }

    /// Register read; `off` is the offset inside the EXI block (0..0x400).
    pub fn read(&mut self, off: u32, size: u32) -> u32 {
        let (c, reg) = (off / STRIDE, off % STRIDE);
        let Some(ch) = self.ch.get(c as usize) else { return 0 };
        let word = match reg & !3 {
            CSR => ch.csr,
            MAR => ch.mar,
            LENGTH => ch.length,
            CR => ch.cr,
            DATA => ch.data,
            _ => 0,
        };
        // narrower accesses read the addressed bytes of the big-endian word
        let end = (reg & 3) + size;
        let shifted = word >> (32 - 8 * end.min(4));
        if size >= 4 {
            word
        } else {
            shifted & ((1u32 << (8 * size)) - 1)
        }
    }

    /// Register write (32-bit); transfers may read or write guest `ram`.
    pub fn write(&mut self, off: u32, value: u32, ram: &mut [u8]) {
        let (c, reg) = (off / STRIDE, off % STRIDE);
        let Some(ch) = self.ch.get_mut(c as usize) else { return };
        match reg {
            CSR => {
                // the EXT bit is read-only (no memory card inserted); interrupt flags clear on write of 1
                let plain = value & !CSR_W1C & !CSR_EXT;
                ch.csr = plain | (ch.csr & CSR_W1C & !(value & CSR_W1C));
                if (ch.csr >> 7) & 7 == 0 {
                    ch.cmd = None;
                    ch.pos = 0;
                }
            }
            MAR => ch.mar = value & 0x03FF_FFE0,
            LENGTH => ch.length = value & 0x03FF_FFE0,
            DATA => ch.data = value,
            CR => {
                ch.cr = value;
                if value & 1 != 0 {
                    self.transfer(c as usize, ram);
                    let ch = &mut self.ch[c as usize];
                    ch.cr &= !1;
                    ch.csr |= CSR_TCINT;
                }
            }
            _ => {}
        }
    }

    fn transfer(&mut self, c: usize, ram: &mut [u8]) {
        let cr = self.ch[c].cr;
        let cs = (self.ch[c].csr >> 7) & 7;
        let rw = (cr >> 2) & 3; // 0 read, 1 write, 2 read/write
        let dma = cr & 2 != 0;
        let len = if dma { self.ch[c].length } else { ((cr >> 4) & 3) + 1 };

        // Only the IPL device (channel 0, device 1) exists; empty slots read as all ones.
        if !(c == 0 && cs == 0b010) {
            if rw != 1 && !dma {
                self.ch[c].data = u32::MAX;
            }
            return;
        }

        if dma {
            let mar = self.ch[c].mar as usize;
            if rw == 0 {
                let bytes = self.device_read(c, len);
                ram[mar..mar + bytes.len()].copy_from_slice(&bytes);
            } else {
                let bytes = ram[mar..mar + len as usize].to_vec();
                self.device_write(c, &bytes);
            }
        } else if rw == 0 {
            let bytes = self.device_read(c, len);
            let mut w = [0u8; 4];
            w[..bytes.len()].copy_from_slice(&bytes);
            self.ch[c].data = u32::from_be_bytes(w);
        } else {
            let w = self.ch[c].data.to_be_bytes();
            self.device_write(c, &w[..len as usize]);
        }
    }

    /// Bytes the IPL device (RTC, SRAM, anything else reads as ones) sends for the current command.
    fn device_read(&mut self, c: usize, len: u32) -> Vec<u8> {
        let (cmd, pos) = (self.ch[c].cmd.unwrap_or(0), self.ch[c].pos as usize);
        self.ch[c].pos += len;
        let source: Vec<u8> = match cmd & 0x7FFF_FFC0 {
            RTC_CMD => self.rtc.to_be_bytes().to_vec(),
            SRAM_CMD => self.sram.to_vec(),
            _ => Vec::new(),
        };
        (0..len as usize).map(|i| source.get(pos + i).copied().unwrap_or(0xFF)).collect()
    }

    fn device_write(&mut self, c: usize, data: &[u8]) {
        let ch = &mut self.ch[c];
        match ch.cmd {
            None => {
                let mut w = [0u8; 4];
                let n = data.len().min(4);
                w[..n].copy_from_slice(&data[..n]);
                ch.cmd = Some(u32::from_be_bytes(w));
                ch.pos = 0;
            }
            Some(cmd) if cmd & CMD_WRITE != 0 && cmd & 0x7FFF_FFC0 == SRAM_CMD => {
                for (i, b) in data.iter().enumerate() {
                    if let Some(s) = self.sram.get_mut(ch.pos as usize + i) {
                        *s = *b;
                    }
                }
                ch.pos += data.len() as u32;
            }
            Some(_) => ch.pos += data.len() as u32,
        }
    }
}

impl Default for Exi {
    fn default() -> Self {
        Exi::new()
    }
}
