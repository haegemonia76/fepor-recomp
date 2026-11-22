//! Vertex decoding and the CPU side of the transform unit.

use crate::cp::{ColorFmt, Comp, Cp, Form, VertexFormat};
use crate::state::Vertex;
use crate::xf::Xf;

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let s = &self.data[self.pos.min(self.data.len())..(self.pos + n).min(self.data.len())];
        self.pos += n;
        s
    }
    fn u8(&mut self) -> u8 {
        self.take(1).first().copied().unwrap_or(0)
    }
    fn index(&mut self, form: Form) -> usize {
        match form {
            Form::Index8 => self.u8() as usize,
            _ => {
                let b = self.take(2);
                if b.len() == 2 {
                    u16::from_be_bytes([b[0], b[1]]) as usize
                } else {
                    0
                }
            }
        }
    }
}

fn read_comp(src: &[u8], at: usize, c: Comp, shift: u32) -> f32 {
    // missing bytes read as zero
    let b = |i: usize| src.get(at + i).copied().unwrap_or(0);
    // 2^-shift, exactly
    let scale = f32::from_bits((127 - shift.min(31)) << 23);
    match c {
        Comp::U8 => b(0) as f32 * scale,
        Comp::I8 => b(0) as i8 as f32 * scale,
        Comp::U16 => u16::from_be_bytes([b(0), b(1)]) as f32 * scale,
        Comp::I16 => i16::from_be_bytes([b(0), b(1)]) as f32 * scale,
        Comp::F32 => f32::from_be_bytes([b(0), b(1), b(2), b(3)]),
    }
}

fn color(src: &[u8], fmt: ColorFmt) -> [f32; 4] {
    let b = |i: usize| src.get(i).copied().unwrap_or(0) as u32;
    let f = |v: u32, max: u32| v as f32 / max as f32;
    match fmt {
        ColorFmt::Rgb565 => {
            let v = b(0) << 8 | b(1);
            [f(v >> 11 & 31, 31), f(v >> 5 & 63, 63), f(v & 31, 31), 1.0]
        }
        ColorFmt::Rgb888 | ColorFmt::Rgb888x => [f(b(0), 255), f(b(1), 255), f(b(2), 255), 1.0],
        ColorFmt::Rgba4444 => {
            let v = b(0) << 8 | b(1);
            [
                f(v >> 12 & 15, 15),
                f(v >> 8 & 15, 15),
                f(v >> 4 & 15, 15),
                f(v & 15, 15),
            ]
        }
        ColorFmt::Rgba6666 => {
            let v = b(0) << 16 | b(1) << 8 | b(2);
            [
                f(v >> 18 & 63, 63),
                f(v >> 12 & 63, 63),
                f(v >> 6 & 63, 63),
                f(v & 63, 63),
            ]
        }
        ColorFmt::Rgba8888 => [f(b(0), 255), f(b(1), 255), f(b(2), 255), f(b(3), 255)],
    }
}

/// Size in bytes of the direct (in-stream) data of one vertex.
pub fn vertex_size(f: &VertexFormat) -> usize {
    let idx = |form: Form| match form {
        Form::Index8 => 1,
        Form::Index16 => 2,
        _ => 0,
    };
    let mut n = f.pos_mtx_idx as usize + f.tex_mtx_idx.iter().filter(|&&b| b).count();
    n += match f.pos {
        Form::Direct => (2 + f.pos_xyz as usize) * f.pos_fmt.size(),
        x => idx(x),
    };
    n += match f.normal {
        Form::Direct => (3 + 6 * f.normal_nbt as usize) * f.normal_fmt.size(),
        x => idx(x),
    };
    for c in 0..2 {
        n += match f.color[c] {
            Form::Direct => f.color_fmt[c].size(),
            x => idx(x),
        };
    }
    for t in 0..8 {
        n += match f.tex[t] {
            Form::Direct => (1 + f.tex_st[t] as usize) * f.tex_fmt[t].size(),
            x => idx(x),
        };
    }
    n
}

fn mul34(m: &[[f32; 4]; 3], v: [f32; 4]) -> [f32; 3] {
    std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2] + m[r][3] * v[3])
}

fn packed_rgba(v: u32) -> [f32; 4] {
    [
        (v >> 24) as f32 / 255.0,
        (v >> 16 & 255) as f32 / 255.0,
        (v >> 8 & 255) as f32 / 255.0,
        (v & 255) as f32 / 255.0,
    ]
}

/// Decode `count` vertices from `data` and run them through XF, into `out` (reused: cleared first).
pub fn decode(
    f: &VertexFormat,
    cp: &Cp,
    xf: &Xf,
    ram: &[u8],
    data: &[u8],
    count: usize,
    mut out: Vec<Vertex>,
) -> Vec<Vertex> {
    let size = vertex_size(f);
    out.clear();
    out.reserve(count);
    let proj = xf.projection();
    let vp = xf.viewport();
    let (zr, zf) = (xf.regf(0x101C), xf.regf(0x101F));
    let num_chan = xf.reg(0x1009).min(2) as usize;
    let num_tex = xf.reg(0x103F).min(8) as usize;

    let array = |n: usize, idx: usize| -> &[u8] {
        let at = (cp.array_base[n] as usize).saturating_add(idx * cp.array_stride[n] as usize);
        ram.get(at..).unwrap_or(&[])
    };

    // what does not change from one vertex to the next, read once per draw
    struct Chan {
        vertex_colour: bool,
        vertex_alpha: bool,
        vertex_ambient: bool,
        lit: bool,
        diffuse_func: u32,
        mat: [f32; 4],
        amb: [f32; 4],
        /// The enabled lights: position (or direction) in view space and colour.
        lights: [([f32; 3], [f32; 3]); 8],
        n_lights: usize,
    }
    let chans: [Chan; 2] = std::array::from_fn(|c| {
        let ctrl = xf.reg(0x100E + c);
        let mask = (ctrl >> 2 & 15) | (ctrl >> 11 & 15) << 4;
        let mut lights = [([0.0f32; 3], [0.0f32; 3]); 8];
        let mut n_lights = 0;
        for l in 0..8 {
            if mask >> l & 1 == 0 {
                continue;
            }
            let lb = 0x600 + l * 16;
            let lc = packed_rgba(xf.mem[lb + 3]);
            lights[n_lights] = (
                [xf.f(lb + 10), xf.f(lb + 11), xf.f(lb + 12)],
                [lc[0], lc[1], lc[2]],
            );
            n_lights += 1;
        }
        Chan {
            vertex_colour: ctrl & 1 != 0,
            vertex_alpha: xf.reg(0x1010 + c) & 1 != 0,
            vertex_ambient: ctrl >> 6 & 1 != 0,
            lit: ctrl >> 1 & 1 != 0,
            diffuse_func: ctrl >> 7 & 3,
            mat: packed_rgba(xf.reg(0x100C + c)),
            amb: packed_rgba(xf.reg(0x100A + c)),
            lights,
            n_lights,
        }
    });
    let lighting = (0..num_chan).any(|c| chans[c].lit);
    struct TexGen {
        stq: bool,
        abc1: bool,
        ty: u32,
        row: u32,
    }
    let texgens: [TexGen; 8] = std::array::from_fn(|n| {
        let info = xf.reg(0x1040 + n.min(7));
        TexGen {
            stq: info >> 1 & 1 != 0,
            abc1: info >> 2 & 1 != 0,
            ty: info >> 4 & 7,
            row: info >> 7 & 31,
        }
    });
    // matrices are read again only when the index changes (a skinned mesh changes it per bone, not per vertex)
    let (mut pm_idx, mut pm) = (usize::MAX, [[0.0f32; 4]; 3]);
    let (mut nm_idx, mut nm) = (usize::MAX, [[0.0f32; 3]; 3]);
    let mut tm_idx = [usize::MAX; 8];
    let mut tm = [[[0.0f32; 4]; 3]; 8];

    for v in 0..count {
        let mut r = Reader {
            data: data.get(v * size..).unwrap_or(&[]),
            pos: 0,
        };
        let pos_mtx = if f.pos_mtx_idx {
            r.u8() as usize & 0x3F
        } else {
            (cp.mat_idx_a & 0x3F) as usize
        };
        let tex_mtx: [usize; 8] = std::array::from_fn(|i| {
            if f.tex_mtx_idx[i] {
                r.u8() as usize & 0x3F
            } else if i < 4 {
                (cp.mat_idx_a >> (6 + 6 * i) & 0x3F) as usize
            } else {
                (cp.mat_idx_b >> (6 * (i - 4)) & 0x3F) as usize
            }
        });

        // position
        let mut p = [0.0f32; 3];
        if f.pos.present() {
            let n = 2 + f.pos_xyz as usize;
            let (src, base): (&[u8], usize) = match f.pos {
                Form::Direct => (r.take(n * f.pos_fmt.size()), 0),
                form => {
                    let idx = r.index(form);
                    let a = array(0, idx);
                    (&a[..(n * f.pos_fmt.size()).min(a.len())], 0)
                }
            };
            for (i, c) in p.iter_mut().enumerate().take(n) {
                *c = read_comp(&src, base + i * f.pos_fmt.size(), f.pos_fmt, f.pos_shift);
            }
        }
        // normal (first of N/NBT only; the others are for bump mapping)
        let mut nrm = [0.0f32; 3];
        if f.normal.present() {
            let comps = 3 + 6 * f.normal_nbt as usize;
            let shift = match f.normal_fmt {
                Comp::I8 | Comp::U8 => 6,
                Comp::I16 | Comp::U16 => 14,
                _ => 0,
            };
            let src: &[u8] = match f.normal {
                Form::Direct => r.take(comps * f.normal_fmt.size()),
                form => {
                    let idx = r.index(form);
                    let a = array(1, idx);
                    &a[..(comps * f.normal_fmt.size()).min(a.len())]
                }
            };
            for (i, c) in nrm.iter_mut().enumerate() {
                *c = read_comp(&src, i * f.normal_fmt.size(), f.normal_fmt, shift);
            }
        }
        // colours
        let mut vcol: [Option<[f32; 4]>; 2] = [None; 2];
        for c in 0..2 {
            if f.color[c].present() {
                let sz = f.color_fmt[c].size();
                let src: &[u8] = match f.color[c] {
                    Form::Direct => r.take(sz),
                    form => {
                        let idx = r.index(form);
                        let a = array(2 + c, idx);
                        &a[..sz.min(a.len())]
                    }
                };
                vcol[c] = Some(color(&src, f.color_fmt[c]));
            }
        }
        // texture coordinates
        let mut tc: [[f32; 3]; 8] = [[0.0, 0.0, 1.0]; 8];
        for t in 0..8 {
            if f.tex[t].present() {
                let n = 1 + f.tex_st[t] as usize;
                let sz = n * f.tex_fmt[t].size();
                let src: &[u8] = match f.tex[t] {
                    Form::Direct => r.take(sz),
                    form => {
                        let idx = r.index(form);
                        let a = array(4 + t, idx);
                        &a[..sz.min(a.len())]
                    }
                };
                for i in 0..n {
                    tc[t][i] =
                        read_comp(&src, i * f.tex_fmt[t].size(), f.tex_fmt[t], f.tex_shift[t]);
                }
            }
        }

        // transform
        if pm_idx != pos_mtx {
            pm = xf.pos_matrix(pos_mtx);
            pm_idx = pos_mtx;
        }
        let view = mul34(&pm, [p[0], p[1], p[2], 1.0]);
        let clip: [f32; 4] = std::array::from_fn(|r| {
            proj[r][0] * view[0] + proj[r][1] * view[1] + proj[r][2] * view[2] + proj[r][3]
        });
        let (w, ndc_z) = (
            clip[3],
            if clip[3] != 0.0 {
                clip[2] / clip[3]
            } else {
                0.0
            },
        );
        let depth = ((ndc_z * zr + zf) / 16_777_215.0).clamp(0.0, 1.0);
        let n_view: [f32; 3] = if lighting {
            if nm_idx != pos_mtx {
                nm = xf.normal_matrix(pos_mtx);
                nm_idx = pos_mtx;
            }
            std::array::from_fn(|r| nm[r][0] * nrm[0] + nm[r][1] * nrm[1] + nm[r][2] * nrm[2])
        } else {
            [0.0; 3]
        };

        let mut vert = Vertex {
            pos: [clip[0], clip[1], depth * w, w],
            ..Vertex::default()
        };
        let _ = vp;

        // colour channels
        for c in 0..2 {
            if c >= num_chan {
                vert.col[c] = [1.0; 4];
                continue;
            }
            let ch = &chans[c];
            let base_rgb = if ch.vertex_colour {
                vcol[c]
                    .map(|v| [v[0], v[1], v[2]])
                    .unwrap_or([ch.mat[0], ch.mat[1], ch.mat[2]])
            } else {
                [ch.mat[0], ch.mat[1], ch.mat[2]]
            };
            let alpha = if ch.vertex_alpha {
                vcol[c].map(|v| v[3]).unwrap_or(ch.mat[3])
            } else {
                ch.mat[3]
            };
            let mut rgb = base_rgb;
            if ch.lit {
                let mut lit = if ch.vertex_ambient {
                    vcol[c]
                        .map(|v| [v[0], v[1], v[2]])
                        .unwrap_or([ch.amb[0], ch.amb[1], ch.amb[2]])
                } else {
                    [ch.amb[0], ch.amb[1], ch.amb[2]]
                };
                for (lpos, lcol) in &ch.lights[..ch.n_lights] {
                    // lpos is the light position (point) or direction (directional) in view space
                    let mut ldir = [lpos[0] - view[0], lpos[1] - view[1], lpos[2] - view[2]];
                    let len = (ldir[0] * ldir[0] + ldir[1] * ldir[1] + ldir[2] * ldir[2])
                        .sqrt()
                        .max(1e-6);
                    ldir = [ldir[0] / len, ldir[1] / len, ldir[2] / len];
                    let ndotl =
                        (n_view[0] * ldir[0] + n_view[1] * ldir[1] + n_view[2] * ldir[2]).max(0.0);
                    let diff = if ch.diffuse_func == 0 { 1.0 } else { ndotl };
                    for k in 0..3 {
                        lit[k] += lcol[k] * diff;
                    }
                }
                rgb = [
                    (lit[0].clamp(0.0, 1.0)) * base_rgb[0],
                    (lit[1].clamp(0.0, 1.0)) * base_rgb[1],
                    (lit[2].clamp(0.0, 1.0)) * base_rgb[2],
                ];
            }
            vert.col[c] = [rgb[0], rgb[1], rgb[2], alpha];
        }

        // texture coordinate generation
        for n in 0..num_tex {
            let g = &texgens[n];
            let src: [f32; 3] = match g.row {
                0 => p,
                1 => nrm,
                2 => [vert.col[0][0], vert.col[0][1], vert.col[0][2]],
                5..=12 => tc[(g.row - 5) as usize],
                _ => [0.0; 3],
            };
            if g.ty != 0 {
                // emboss/colour generation is not modelled: pass the source through
                vert.uv[n] = [src[0], src[1], 1.0];
                continue;
            }
            let input = [src[0], src[1], if g.abc1 { src[2] } else { 1.0 }, 1.0];
            if tm_idx[n] != tex_mtx[n] {
                tm[n] = xf.pos_matrix(tex_mtx[n]);
                tm_idx[n] = tex_mtx[n];
            }
            let tmx = &tm[n];
            let s = tmx[0][0] * input[0]
                + tmx[0][1] * input[1]
                + tmx[0][2] * input[2]
                + tmx[0][3] * input[3];
            let t = tmx[1][0] * input[0]
                + tmx[1][1] * input[1]
                + tmx[1][2] * input[2]
                + tmx[1][3] * input[3];
            let q = if g.stq {
                tmx[2][0] * input[0]
                    + tmx[2][1] * input[1]
                    + tmx[2][2] * input[2]
                    + tmx[2][3] * input[3]
            } else {
                1.0
            };
            vert.uv[n] = [s, t, q];
        }
        out.push(vert);
    }
    out
}
