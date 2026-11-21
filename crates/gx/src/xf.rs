//! Transform unit: matrix memory and registers.

pub const MEM_WORDS: usize = 0x1000;
pub const REG_BASE: usize = 0x1000;

#[derive(Clone)]
pub struct Xf {
    pub mem: Vec<u32>,
    pub regs: [u32; 0x60],
}

impl Default for Xf {
    fn default() -> Self {
        Xf {
            mem: vec![0; MEM_WORDS],
            regs: [0; 0x60],
        }
    }
}

impl Xf {
    pub fn write(&mut self, addr: usize, value: u32) {
        if addr < MEM_WORDS {
            self.mem[addr] = value;
        } else if (REG_BASE..REG_BASE + 0x60).contains(&addr) {
            self.regs[addr - REG_BASE] = value;
        }
    }

    pub fn f(&self, addr: usize) -> f32 {
        f32::from_bits(self.mem[addr])
    }

    pub fn reg(&self, addr: usize) -> u32 {
        self.regs[addr - REG_BASE]
    }

    pub fn regf(&self, addr: usize) -> f32 {
        f32::from_bits(self.reg(addr))
    }

    /// Position matrix `idx` (3 rows of 4).
    pub fn pos_matrix(&self, idx: usize) -> [[f32; 4]; 3] {
        let b = (idx & 0x3F) * 4;
        std::array::from_fn(|r| std::array::from_fn(|c| self.f(b + r * 4 + c)))
    }

    /// Normal matrix `idx` (3x3 at 0x400).
    pub fn normal_matrix(&self, idx: usize) -> [[f32; 3]; 3] {
        let b = 0x400 + (idx & 0x1F) * 3;
        std::array::from_fn(|r| std::array::from_fn(|c| self.f(b + r * 3 + c)))
    }

    /// Projection matrix (4x4), perspective or orthographic.
    pub fn projection(&self) -> [[f32; 4]; 4] {
        let p: [f32; 6] = std::array::from_fn(|i| self.regf(0x1020 + i));
        if self.reg(0x1026) == 0 {
            [
                [p[0], 0.0, p[1], 0.0],
                [0.0, p[2], p[3], 0.0],
                [0.0, 0.0, p[4], p[5]],
                [0.0, 0.0, -1.0, 0.0],
            ]
        } else {
            [
                [p[0], 0.0, 0.0, p[1]],
                [0.0, p[2], 0.0, p[3]],
                [0.0, 0.0, p[4], p[5]],
                [0.0, 0.0, 0.0, 1.0],
            ]
        }
    }

    /// Viewport as (x, y, w, h, near, far) in EFB pixels and depth in 0..1.
    pub fn viewport(&self) -> [f32; 6] {
        let (wd, ht, zr, xo, yo, zf) = (
            self.regf(0x101A),
            self.regf(0x101B),
            self.regf(0x101C),
            self.regf(0x101D),
            self.regf(0x101E),
            self.regf(0x101F),
        );
        let (cx, cy) = (xo - 342.0, yo - 342.0);
        let far = zf / 16_777_215.0;
        let near = (zf - zr) / 16_777_215.0;
        [cx - wd, cy + ht, 2.0 * wd, -2.0 * ht, near, far]
    }
}
