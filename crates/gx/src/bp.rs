//! Blitter/raster registers: TEV, textures, blending, depth, scissor, copy.

use crate::state::{RenderState, TevStage, TextureDesc};
use crate::xf::Xf;

pub const GENMODE: usize = 0x00;
pub const SCISSOR_TL: usize = 0x20;
pub const SCISSOR_BR: usize = 0x21;
pub const TREF: usize = 0x28;
pub const ZMODE: usize = 0x40;
pub const BLENDMODE: usize = 0x41;
pub const CONSTANT_ALPHA: usize = 0x42;
pub const SET_DRAW_DONE: usize = 0x45;
pub const PE_TOKEN: usize = 0x47;
pub const PE_TOKEN_INT: usize = 0x48;
pub const EFB_TL: usize = 0x49;
pub const EFB_WH: usize = 0x4A;
pub const EFB_ADDR: usize = 0x4B;
pub const EFB_STRIDE: usize = 0x4D;
pub const CLEAR_AR: usize = 0x4F;
pub const CLEAR_GB: usize = 0x50;
pub const CLEAR_Z: usize = 0x51;
pub const TRIGGER_COPY: usize = 0x52;
pub const SCISSOR_OFFSET: usize = 0x59;
pub const LOAD_TLUT0: usize = 0x64;
pub const LOAD_TLUT1: usize = 0x65;
pub const TEV_COLOR_ENV: usize = 0xC0;
pub const TEV_REG: usize = 0xE0;
pub const ALPHA_COMPARE: usize = 0xF3;
pub const TEV_KSEL: usize = 0xF6;

pub struct Bp {
    pub regs: [u32; 256],
    pub tev_regs: [[i32; 4]; 4],
    pub konst: [[u8; 4]; 4],
}

impl Default for Bp {
    fn default() -> Self {
        Bp {
            regs: [0; 256],
            tev_regs: [[0; 4]; 4],
            konst: [[0; 4]; 4],
        }
    }
}

fn sext11(v: u32) -> i32 {
    ((v & 0x7FF) as i32) << 21 >> 21
}

impl Bp {
    /// Store a register write. The TEV colour registers are written in R/A and B/G halves.
    pub fn write(&mut self, reg: usize, value: u32) {
        let value = value & 0x00FF_FFFF;
        self.regs[reg] = value;
        if (TEV_REG..TEV_REG + 8).contains(&reg) {
            let i = (reg - TEV_REG) / 2;
            let (lo, hi) = (sext11(value), sext11(value >> 12));
            let konst = value >> 23 & 1 != 0;
            let second = reg & 1 != 0;
            if konst {
                let c = |v: i32| v.clamp(0, 255) as u8;
                if second {
                    self.konst[i][2] = c(lo);
                    self.konst[i][1] = c(hi);
                } else {
                    self.konst[i][0] = c(lo);
                    self.konst[i][3] = c(hi);
                }
            } else if second {
                self.tev_regs[i][2] = lo;
                self.tev_regs[i][1] = hi;
            } else {
                self.tev_regs[i][0] = lo;
                self.tev_regs[i][3] = hi;
            }
        }
    }

    fn tex_regs(&self, map: usize) -> (u32, u32, u32, u32, u32) {
        let (m0, m1, i0, i3, tl) = if map < 4 {
            (0x80, 0x84, 0x88, 0x94, 0x98)
        } else {
            (0xA0, 0xA4, 0xA8, 0xB4, 0xB8)
        };
        let n = map & 3;
        (
            self.regs[m0 + n],
            self.regs[m1 + n],
            self.regs[i0 + n],
            self.regs[i3 + n],
            self.regs[tl + n],
        )
    }

    /// Snapshot everything a draw depends on.
    pub fn render_state(&self, xf: &Xf) -> RenderState {
        let gen = self.regs[GENMODE];
        let mut s = RenderState {
            num_tex_gens: gen & 15,
            num_colors: gen >> 4 & 7,
            num_tev_stages: (gen >> 10 & 15) + 1,
            cull: (gen >> 14 & 3) as u8,
            tev_regs: self.tev_regs,
            konst: self.konst,
            alpha_compare: self.regs[ALPHA_COMPARE],
            z_mode: self.regs[ZMODE],
            blend_mode: self.regs[BLENDMODE],
            constant_alpha: self.regs[CONSTANT_ALPHA],
            color_update: self.regs[BLENDMODE] >> 3 & 1 != 0,
            alpha_update: self.regs[BLENDMODE] >> 4 & 1 != 0,
            viewport: xf.viewport(),
            ..RenderState::default()
        };
        // scissor: top-left and bottom-right are inclusive and carry a +342 bias; the offset is in units of 2
        let (tl, br) = (self.regs[SCISSOR_TL], self.regs[SCISSOR_BR]);
        let off = self.regs[SCISSOR_OFFSET];
        let (ox, oy) = (
            ((off & 0x1FF) as i32 - 171) * 2,
            ((off >> 10 & 0x1FF) as i32 - 171) * 2,
        );
        let (x0, y0) = (
            (tl >> 12 & 0x7FF) as i32 - 342 - ox,
            (tl & 0x7FF) as i32 - 342 - oy,
        );
        let (x1, y1) = (
            (br >> 12 & 0x7FF) as i32 - 342 - ox,
            (br & 0x7FF) as i32 - 342 - oy,
        );
        s.scissor = [x0, y0, (x1 - x0 + 1).max(0), (y1 - y0 + 1).max(0)];

        // swap tables: KSEL registers 0..7 hold table n/2 entries (even: r,g; odd: b,a)
        for t in 0..4 {
            let (even, odd) = (self.regs[TEV_KSEL + 2 * t], self.regs[TEV_KSEL + 2 * t + 1]);
            s.swaps[t] = [
                (even & 3) as u8,
                (even >> 2 & 3) as u8,
                (odd & 3) as u8,
                (odd >> 2 & 3) as u8,
            ];
        }
        for n in 0..16 {
            let order = self.regs[TREF + n / 2];
            let ksel = self.regs[TEV_KSEL + n / 2];
            let (shift, ks) = if n & 1 == 0 {
                (0, ksel >> 4)
            } else {
                (12, ksel >> 14)
            };
            s.tev[n] = TevStage {
                color_env: self.regs[TEV_COLOR_ENV + 2 * n],
                alpha_env: self.regs[TEV_COLOR_ENV + 2 * n + 1],
                texmap: (order >> shift & 7) as u8,
                texcoord: (order >> (shift + 3) & 7) as u8,
                tex_enable: order >> (shift + 6) & 1 != 0,
                chan: (order >> (shift + 7) & 7) as u8,
                konst_color: (ks & 31) as u8,
                konst_alpha: (ks >> 5 & 31) as u8,
            };
        }
        for m in 0..8 {
            let (mode0, _mode1, img0, img3, tlut) = self.tex_regs(m);
            s.textures[m] = TextureDesc {
                addr: (img3 & 0x00FF_FFFF) << 5,
                width: (img0 & 0x3FF) + 1,
                height: (img0 >> 10 & 0x3FF) + 1,
                format: (img0 >> 20 & 15) as u8,
                wrap_s: (mode0 & 3) as u8,
                wrap_t: (mode0 >> 2 & 3) as u8,
                mag_linear: mode0 >> 4 & 1 != 0,
                min_linear: mode0 >> 7 & 1 != 0,
                tlut_offset: tlut & 0x3FF,
                tlut_format: (tlut >> 10 & 3) as u8,
            };
        }
        s
    }
}
