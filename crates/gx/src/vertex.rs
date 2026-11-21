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
    let g = |n: usize| src.get(at..at + n).unwrap_or(&[0; 4][..n]).to_vec();
    let scale = if c == Comp::F32 {
        1.0
    } else {
        1.0 / (1u32 << shift) as f32
    };
    match c {
        Comp::U8 => g(1)[0] as f32 * scale,
        Comp::I8 => g(1)[0] as i8 as f32 * scale,
        Comp::U16 => u16::from_be_bytes(g(2).try_into().unwrap()) as f32 * scale,
        Comp::I16 => i16::from_be_bytes(g(2).try_into().unwrap()) as f32 * scale,
        Comp::F32 => f32::from_be_bytes(g(4).try_into().unwrap()),
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

/// Decode `count` vertices from `data` and run them through XF.
pub fn decode(
    f: &VertexFormat,
    cp: &Cp,
    xf: &Xf,
    ram: &[u8],
    data: &[u8],
    count: usize,
) -> Vec<Vertex> {
    let size = vertex_size(f);
    let mut out = Vec::with_capacity(count);
    let proj = xf.projection();
    let vp = xf.viewport();
    let (zr, zf) = (xf.regf(0x101C), xf.regf(0x101F));
    let num_chan = xf.reg(0x1009).min(2) as usize;
    let num_tex = xf.reg(0x103F).min(8) as usize;

    let array = |n: usize, idx: usize| -> &[u8] {
        let at = (cp.array_base[n] as usize).saturating_add(idx * cp.array_stride[n] as usize);
        ram.get(at..).unwrap_or(&[])
    };

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
            let (src, base): (Vec<u8>, usize) = match f.pos {
                Form::Direct => (r.take(n * f.pos_fmt.size()).to_vec(), 0),
                form => {
                    let idx = r.index(form);
                    (
                        array(0, idx)[..(n * f.pos_fmt.size()).min(array(0, idx).len())].to_vec(),
                        0,
                    )
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
            let src: Vec<u8> = match f.normal {
                Form::Direct => r.take(comps * f.normal_fmt.size()).to_vec(),
                form => {
                    let idx = r.index(form);
                    let a = array(1, idx);
                    a[..(comps * f.normal_fmt.size()).min(a.len())].to_vec()
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
                let src: Vec<u8> = match f.color[c] {
                    Form::Direct => r.take(sz).to_vec(),
                    form => {
                        let idx = r.index(form);
                        let a = array(2 + c, idx);
                        a[..sz.min(a.len())].to_vec()
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
                let src: Vec<u8> = match f.tex[t] {
                    Form::Direct => r.take(sz).to_vec(),
                    form => {
                        let idx = r.index(form);
                        let a = array(4 + t, idx);
                        a[..sz.min(a.len())].to_vec()
                    }
                };
                for i in 0..n {
                    tc[t][i] =
                        read_comp(&src, i * f.tex_fmt[t].size(), f.tex_fmt[t], f.tex_shift[t]);
                }
            }
        }

        // transform
        let pm = xf.pos_matrix(pos_mtx);
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
        let nm = xf.normal_matrix(pos_mtx);
        let n_view: [f32; 3] =
            std::array::from_fn(|r| nm[r][0] * nrm[0] + nm[r][1] * nrm[1] + nm[r][2] * nrm[2]);

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
            let ctrl = xf.reg(0x100E + c);
            let actrl = xf.reg(0x1010 + c);
            let mat = packed_rgba(xf.reg(0x100C + c));
            let amb = packed_rgba(xf.reg(0x100A + c));
            let base_rgb = if ctrl & 1 != 0 {
                vcol[c]
                    .map(|v| [v[0], v[1], v[2]])
                    .unwrap_or([mat[0], mat[1], mat[2]])
            } else {
                [mat[0], mat[1], mat[2]]
            };
            let alpha = if actrl & 1 != 0 {
                vcol[c].map(|v| v[3]).unwrap_or(mat[3])
            } else {
                mat[3]
            };
            let mut rgb = base_rgb;
            if ctrl >> 1 & 1 != 0 {
                let amb_rgb = if ctrl >> 6 & 1 != 0 {
                    vcol[c]
                        .map(|v| [v[0], v[1], v[2]])
                        .unwrap_or([amb[0], amb[1], amb[2]])
                } else {
                    [amb[0], amb[1], amb[2]]
                };
                let mask = (ctrl >> 2 & 15) | (ctrl >> 11 & 15) << 4;
                let mut lit = amb_rgb;
                for l in 0..8 {
                    if mask >> l & 1 == 0 {
                        continue;
                    }
                    let lb = 0x600 + l * 16;
                    let lc = packed_rgba(xf.mem[lb + 3]);
                    // dpos holds the light position (point) or direction (directional) in view space
                    let d = [xf.f(lb + 10), xf.f(lb + 11), xf.f(lb + 12)];
                    let mut ldir = [d[0] - view[0], d[1] - view[1], d[2] - view[2]];
                    let len = (ldir[0] * ldir[0] + ldir[1] * ldir[1] + ldir[2] * ldir[2])
                        .sqrt()
                        .max(1e-6);
                    ldir = [ldir[0] / len, ldir[1] / len, ldir[2] / len];
                    let ndotl =
                        (n_view[0] * ldir[0] + n_view[1] * ldir[1] + n_view[2] * ldir[2]).max(0.0);
                    let dfunc = ctrl >> 7 & 3;
                    let diff = if dfunc == 0 { 1.0 } else { ndotl };
                    for k in 0..3 {
                        lit[k] += lc[k] * diff;
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
            let info = xf.reg(0x1040 + n);
            let stq = info >> 1 & 1 != 0;
            let abc1 = info >> 2 & 1 != 0;
            let ty = info >> 4 & 7;
            let row = info >> 7 & 31;
            let src: [f32; 3] = match row {
                0 => p,
                1 => nrm,
                2 => [vert.col[0][0], vert.col[0][1], vert.col[0][2]],
                5..=12 => tc[(row - 5) as usize],
                _ => [0.0; 3],
            };
            if ty != 0 {
                // emboss/colour generation is not modelled: pass the source through
                vert.uv[n] = [src[0], src[1], 1.0];
                continue;
            }
            let input = [src[0], src[1], if abc1 { src[2] } else { 1.0 }, 1.0];
            let tm = xf.pos_matrix(tex_mtx[n]);
            let s = tm[0][0] * input[0]
                + tm[0][1] * input[1]
                + tm[0][2] * input[2]
                + tm[0][3] * input[3];
            let t = tm[1][0] * input[0]
                + tm[1][1] * input[1]
                + tm[1][2] * input[2]
                + tm[1][3] * input[3];
            let q = if stq {
                tm[2][0] * input[0]
                    + tm[2][1] * input[1]
                    + tm[2][2] * input[2]
                    + tm[2][3] * input[3]
            } else {
                1.0
            };
            vert.uv[n] = [s, t, q];
        }
        out.push(vert);
    }
    out
}
