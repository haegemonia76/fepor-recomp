//! Texture decoding: GX tiled formats to RGBA8.

pub const I4: u8 = 0;
pub const I8: u8 = 1;
pub const IA4: u8 = 2;
pub const IA8: u8 = 3;
pub const RGB565: u8 = 4;
pub const RGB5A3: u8 = 5;
pub const RGBA8: u8 = 6;
pub const C4: u8 = 8;
pub const C8: u8 = 9;
pub const C14X2: u8 = 10;
pub const CMPR: u8 = 14;

/// Tile size in texels for a format.
fn block(format: u8) -> (u32, u32) {
    match format {
        I4 | C4 | CMPR => (8, 8),
        I8 | IA4 | C8 => (8, 4),
        _ => (4, 4),
    }
}

/// Bytes the image occupies in memory (blocks are padded to whole tiles).
pub fn image_size(format: u8, w: u32, h: u32) -> usize {
    let (bw, bh) = block(format);
    let blocks = (w.div_ceil(bw) * h.div_ceil(bh)) as usize;
    let per_block = match format {
        I4 | C4 | CMPR => 32,
        I8 | IA4 | C8 | IA8 | RGB565 | RGB5A3 | C14X2 => 32,
        RGBA8 => 64,
        _ => 32,
    };
    blocks * per_block
}

fn rgb565(v: u16) -> [u8; 4] {
    let (r, g, b) = ((v >> 11 & 31) as u32, (v >> 5 & 63) as u32, (v & 31) as u32);
    [
        (r * 255 / 31) as u8,
        (g * 255 / 63) as u8,
        (b * 255 / 31) as u8,
        255,
    ]
}

fn rgb5a3(v: u16) -> [u8; 4] {
    if v & 0x8000 != 0 {
        let (r, g, b) = ((v >> 10 & 31) as u32, (v >> 5 & 31) as u32, (v & 31) as u32);
        [
            (r * 255 / 31) as u8,
            (g * 255 / 31) as u8,
            (b * 255 / 31) as u8,
            255,
        ]
    } else {
        let (a, r, g, b) = (
            (v >> 12 & 7) as u32,
            (v >> 8 & 15) as u32,
            (v >> 4 & 15) as u32,
            (v & 15) as u32,
        );
        [
            (r * 255 / 15) as u8,
            (g * 255 / 15) as u8,
            (b * 255 / 15) as u8,
            (a * 255 / 7) as u8,
        ]
    }
}

fn palette_entry(tlut: &[u8], fmt: u8, i: usize) -> [u8; 4] {
    let v = tlut
        .get(2 * i..2 * i + 2)
        .map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
    match fmt {
        0 => [
            (v & 255) as u8,
            (v & 255) as u8,
            (v & 255) as u8,
            (v >> 8) as u8,
        ], // IA8: alpha high, intensity low
        1 => rgb565(v),
        _ => rgb5a3(v),
    }
}

/// Decode `data` (the tiled image) into RGBA8, row-major, `w * h * 4` bytes.
pub fn decode(format: u8, w: u32, h: u32, data: &[u8], tlut: Option<(&[u8], u8)>) -> Vec<u8> {
    let (bw, bh) = block(format);
    let mut out = vec![0u8; (w * h * 4) as usize];
    let mut put = |x: u32, y: u32, px: [u8; 4]| {
        if x < w && y < h {
            let o = ((y * w + x) * 4) as usize;
            out[o..o + 4].copy_from_slice(&px);
        }
    };
    let byte = |i: usize| data.get(i).copied().unwrap_or(0);
    let be16 = |i: usize| u16::from_be_bytes([byte(i), byte(i + 1)]);
    let mut off = 0usize;
    for by in 0..h.div_ceil(bh) {
        for bx in 0..w.div_ceil(bw) {
            let (x0, y0) = (bx * bw, by * bh);
            match format {
                I4 | C4 => {
                    for y in 0..8 {
                        for x in 0..8 {
                            let b = byte(off + (y * 4 + x / 2) as usize);
                            let v = if x % 2 == 0 { b >> 4 } else { b & 15 };
                            let px = if format == I4 {
                                let i = v * 17;
                                [i, i, i, i]
                            } else {
                                tlut.map_or([v * 17, v * 17, v * 17, 255], |(t, f)| {
                                    palette_entry(t, f, v as usize)
                                })
                            };
                            put(x0 + x, y0 + y, px);
                        }
                    }
                    off += 32;
                }
                I8 | C8 => {
                    for y in 0..4 {
                        for x in 0..8 {
                            let v = byte(off + (y * 8 + x) as usize);
                            let px = if format == I8 {
                                [v, v, v, v]
                            } else {
                                tlut.map_or([v, v, v, 255], |(t, f)| {
                                    palette_entry(t, f, v as usize)
                                })
                            };
                            put(x0 + x, y0 + y, px);
                        }
                    }
                    off += 32;
                }
                IA4 => {
                    for y in 0..4 {
                        for x in 0..8 {
                            let v = byte(off + (y * 8 + x) as usize);
                            let (a, i) = ((v >> 4) * 17, (v & 15) * 17);
                            put(x0 + x, y0 + y, [i, i, i, a]);
                        }
                    }
                    off += 32;
                }
                IA8 => {
                    for y in 0..4 {
                        for x in 0..4 {
                            let o = off + ((y * 4 + x) * 2) as usize;
                            let (a, i) = (byte(o), byte(o + 1));
                            put(x0 + x, y0 + y, [i, i, i, a]);
                        }
                    }
                    off += 32;
                }
                RGB565 | RGB5A3 | C14X2 => {
                    for y in 0..4 {
                        for x in 0..4 {
                            let v = be16(off + ((y * 4 + x) * 2) as usize);
                            let px = match format {
                                RGB565 => rgb565(v),
                                RGB5A3 => rgb5a3(v),
                                _ => tlut.map_or([0, 0, 0, 255], |(t, f)| {
                                    palette_entry(t, f, (v & 0x3FFF) as usize)
                                }),
                            };
                            put(x0 + x, y0 + y, px);
                        }
                    }
                    off += 32;
                }
                RGBA8 => {
                    for y in 0..4 {
                        for x in 0..4 {
                            let i = ((y * 4 + x) * 2) as usize;
                            let (a, r) = (byte(off + i), byte(off + i + 1));
                            let (g, b) = (byte(off + 32 + i), byte(off + 32 + i + 1));
                            put(x0 + x, y0 + y, [r, g, b, a]);
                        }
                    }
                    off += 64;
                }
                CMPR => {
                    for sub in 0..4u32 {
                        let (sx, sy) = ((sub % 2) * 4, (sub / 2) * 4);
                        let o = off + (sub * 8) as usize;
                        let (c0, c1) = (be16(o), be16(o + 2));
                        let (p0, p1) = (rgb565(c0), rgb565(c1));
                        let mix = |a: [u8; 4], b: [u8; 4], wa: u32, wb: u32| -> [u8; 4] {
                            let d = wa + wb;
                            std::array::from_fn(|k| {
                                if k == 3 {
                                    255
                                } else {
                                    ((a[k] as u32 * wa + b[k] as u32 * wb) / d) as u8
                                }
                            })
                        };
                        let pal = if c0 > c1 {
                            [p0, p1, mix(p0, p1, 2, 1), mix(p0, p1, 1, 2)]
                        } else {
                            [p0, p1, mix(p0, p1, 1, 1), [0, 0, 0, 0]]
                        };
                        for y in 0..4u32 {
                            let row = byte(o + 4 + y as usize);
                            for x in 0..4u32 {
                                let idx = (row >> (6 - 2 * x)) & 3;
                                put(x0 + sx + x, y0 + sy + y, pal[idx as usize]);
                            }
                        }
                    }
                    off += 32;
                }
                _ => {
                    for y in 0..bh {
                        for x in 0..bw {
                            put(x0 + x, y0 + y, [255, 0, 255, 255]); // unsupported format marker
                        }
                    }
                    off += 32;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_whole_tiles() {
        assert_eq!(image_size(RGBA8, 4, 4), 64);
        assert_eq!(image_size(RGB565, 5, 5), 4 * 32);
        assert_eq!(image_size(CMPR, 8, 8), 32);
        assert_eq!(image_size(I4, 16, 8), 64);
    }

    #[test]
    fn rgb565_and_rgba8_tile_layout() {
        // 4x4 RGB565 tile with a distinguishable first and last texel
        let mut tile = vec![0u8; 32];
        tile[0..2].copy_from_slice(&0xF800u16.to_be_bytes()); // red at (0,0)
        tile[30..32].copy_from_slice(&0x001Fu16.to_be_bytes()); // blue at (3,3)
        let px = decode(RGB565, 4, 4, &tile, None);
        assert_eq!(&px[0..4], &[255, 0, 0, 255]);
        assert_eq!(&px[(3 * 4 + 3) * 4..][..4], &[0, 0, 255, 255]);

        // RGBA8 stores AR then GB halves; texel (1,0) is the second pair
        let mut t = vec![0u8; 64];
        t[2..4].copy_from_slice(&[0x80, 0x11]); // a=0x80, r=0x11
        t[34..36].copy_from_slice(&[0x22, 0x33]); // g, b
        let px = decode(RGBA8, 4, 4, &t, None);
        assert_eq!(&px[4..8], &[0x11, 0x22, 0x33, 0x80]);
    }

    #[test]
    fn cmpr_block_uses_four_colors_or_transparent() {
        let mut d = vec![0u8; 32];
        d[0..2].copy_from_slice(&0xFFFFu16.to_be_bytes()); // c0 white
        d[2..4].copy_from_slice(&0x0000u16.to_be_bytes()); // c1 black, c0 > c1: opaque 4-colour mode
        d[4] = 0b00_01_10_11; // row 0: indices 0,1,2,3 left to right = c0, c1, 2/3c0+1/3c1, 1/3c0+2/3c1
        let px = decode(CMPR, 8, 8, &d, None);
        assert_eq!(&px[0..4], &[255, 255, 255, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 255]);
        assert_eq!(px[8], 170);
        assert_eq!(px[12], 85);
    }
}
