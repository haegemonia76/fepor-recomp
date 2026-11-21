//! Command processor registers: vertex descriptor, vertex attribute tables, array bases and strides.

#[derive(Clone, Default)]
pub struct Cp {
    pub vcd_lo: u32,
    pub vcd_hi: u32,
    pub vat_a: [u32; 8],
    pub vat_b: [u32; 8],
    pub vat_c: [u32; 8],
    pub array_base: [u32; 16],
    pub array_stride: [u32; 16],
    pub mat_idx_a: u32,
    pub mat_idx_b: u32,
}

impl Cp {
    pub fn load(&mut self, reg: u8, value: u32) {
        let n = (reg & 0x0F) as usize;
        match reg & 0xF0 {
            0x30 => self.mat_idx_a = value,
            0x40 => self.mat_idx_b = value,
            0x50 => self.vcd_lo = value,
            0x60 => self.vcd_hi = value,
            0x70 => self.vat_a[n & 7] = value,
            0x80 => self.vat_b[n & 7] = value,
            0x90 => self.vat_c[n & 7] = value,
            0xA0 => self.array_base[n] = value & 0x03FF_FFFF,
            0xB0 => self.array_stride[n] = value & 0xFF,
            _ => {}
        }
    }
}

/// How a vertex attribute is supplied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    None,
    Direct,
    Index8,
    Index16,
}

impl Form {
    pub fn from_bits(b: u32) -> Form {
        match b & 3 {
            0 => Form::None,
            1 => Form::Direct,
            2 => Form::Index8,
            _ => Form::Index16,
        }
    }
    pub fn present(self) -> bool {
        self != Form::None
    }
}

/// Component storage format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Comp {
    U8,
    I8,
    U16,
    I16,
    F32,
}

impl Comp {
    pub fn from_bits(b: u32) -> Comp {
        match b & 7 {
            0 => Comp::U8,
            1 => Comp::I8,
            2 => Comp::U16,
            3 => Comp::I16,
            _ => Comp::F32,
        }
    }
    pub fn size(self) -> usize {
        match self {
            Comp::U8 | Comp::I8 => 1,
            Comp::U16 | Comp::I16 => 2,
            Comp::F32 => 4,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorFmt {
    Rgb565,
    Rgb888,
    Rgb888x,
    Rgba4444,
    Rgba6666,
    Rgba8888,
}

impl ColorFmt {
    pub fn from_bits(b: u32) -> ColorFmt {
        match b & 7 {
            0 => ColorFmt::Rgb565,
            1 => ColorFmt::Rgb888,
            2 => ColorFmt::Rgb888x,
            3 => ColorFmt::Rgba4444,
            4 => ColorFmt::Rgba6666,
            _ => ColorFmt::Rgba8888,
        }
    }
    pub fn size(self) -> usize {
        match self {
            ColorFmt::Rgb565 | ColorFmt::Rgba4444 => 2,
            ColorFmt::Rgb888 | ColorFmt::Rgba6666 => 3,
            ColorFmt::Rgb888x | ColorFmt::Rgba8888 => 4,
        }
    }
}

/// Everything that decides the layout of one vertex, decoded from VCD and one VAT entry.
#[derive(Clone, Debug)]
pub struct VertexFormat {
    pub pos_mtx_idx: bool,
    pub tex_mtx_idx: [bool; 8],
    pub pos: Form,
    pub pos_xyz: bool,
    pub pos_fmt: Comp,
    pub pos_shift: u32,
    pub normal: Form,
    pub normal_nbt: bool,
    pub normal_fmt: Comp,
    pub color: [Form; 2],
    pub color_alpha: [bool; 2],
    pub color_fmt: [ColorFmt; 2],
    pub tex: [Form; 8],
    pub tex_st: [bool; 8],
    pub tex_fmt: [Comp; 8],
    pub tex_shift: [u32; 8],
}

impl VertexFormat {
    pub fn new(cp: &Cp, vat: usize) -> VertexFormat {
        let (lo, hi) = (cp.vcd_lo, cp.vcd_hi);
        let (a, b, c) = (cp.vat_a[vat & 7], cp.vat_b[vat & 7], cp.vat_c[vat & 7]);
        let mut f = VertexFormat {
            pos_mtx_idx: lo & 1 != 0,
            tex_mtx_idx: std::array::from_fn(|i| lo >> (1 + i) & 1 != 0),
            pos: Form::from_bits(lo >> 9),
            pos_xyz: a & 1 != 0,
            pos_fmt: Comp::from_bits(a >> 1),
            pos_shift: a >> 4 & 31,
            normal: Form::from_bits(lo >> 11),
            normal_nbt: a >> 9 & 1 != 0,
            normal_fmt: Comp::from_bits(a >> 10),
            color: [Form::from_bits(lo >> 13), Form::from_bits(lo >> 15)],
            color_alpha: [a >> 13 & 1 != 0, a >> 17 & 1 != 0],
            color_fmt: [ColorFmt::from_bits(a >> 14), ColorFmt::from_bits(a >> 18)],
            tex: std::array::from_fn(|i| Form::from_bits(hi >> (2 * i))),
            tex_st: [false; 8],
            tex_fmt: [Comp::F32; 8],
            tex_shift: [0; 8],
        };
        // texture coordinate 0 lives in VAT A; 1..3 in B; 4..7 in B/C
        f.tex_st[0] = a >> 21 & 1 != 0;
        f.tex_fmt[0] = Comp::from_bits(a >> 22);
        f.tex_shift[0] = a >> 25 & 31;
        let b_fields = [(0, 1, 4), (9, 10, 13), (18, 19, 22)];
        for (i, (cnt, fmt, sh)) in b_fields.into_iter().enumerate() {
            f.tex_st[1 + i] = b >> cnt & 1 != 0;
            f.tex_fmt[1 + i] = Comp::from_bits(b >> fmt);
            f.tex_shift[1 + i] = b >> sh & 31;
        }
        f.tex_st[4] = b >> 27 & 1 != 0;
        f.tex_fmt[4] = Comp::from_bits(b >> 28);
        f.tex_shift[4] = c & 31;
        let c_fields = [(5, 6, 9), (14, 15, 18), (23, 24, 27)];
        for (i, (cnt, fmt, sh)) in c_fields.into_iter().enumerate() {
            f.tex_st[5 + i] = c >> cnt & 1 != 0;
            f.tex_fmt[5 + i] = Comp::from_bits(c >> fmt);
            f.tex_shift[5 + i] = c >> sh & 31;
        }
        f
    }
}
