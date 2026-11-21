//! Guest CPU state. The layout mirrors `fepor::Context` in `runtime/include/fepor/ppc.h`
//! (checked there by a static_assert and here by `LAYOUT_CHECK`).

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Crf {
    pub lt: u8,
    pub gt: u8,
    pub eq: u8,
    pub so: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Xer {
    pub so: u8,
    pub ov: u8,
    pub ca: u8,
    pub bc: u8,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct Fpr {
    pub ps0: f64,
    pub ps1: f64,
}

#[repr(C)]
pub struct Context {
    pub r: [u32; 32],
    pub f: [Fpr; 32],
    pub cr: [Crf; 8],
    pub lr: u32,
    pub ctr: u32,
    pub xer: Xer,
    pub fpscr: u32,
    pub msr: u32,
    pub spr: [u32; 1024],
    pub sr: [u32; 16],
    pub reserve_addr: u32,
    pub ram: *mut u8,
}

const _LAYOUT_CHECK: () = {
    assert!(std::mem::size_of::<Context>() == 4864);
    assert!(std::mem::offset_of!(Context, ram) == 4856);
    assert!(std::mem::offset_of!(Context, f) == 128);
};

/// SPR numbers used by the runtime.
pub mod spr {
    pub const PVR: usize = 287;
}

impl Context {
    pub fn new(ram: *mut u8) -> Box<Context> {
        // SAFETY: every field is plain data for which all-zero is a valid value.
        let mut c: Box<Context> = unsafe { Box::new(std::mem::zeroed()) };
        c.ram = ram;
        c.spr[spr::PVR] = 0x0008_3214; // Gekko
        c
    }
}

/// CR as the architected 32-bit value (field 0 in the top nibble).
pub fn pack_cr(cr: &[Crf; 8]) -> u32 {
    cr.iter().fold(0, |v, f| {
        v << 4 | (f.lt as u32) << 3 | (f.gt as u32) << 2 | (f.eq as u32) << 1 | f.so as u32
    })
}

pub fn unpack_cr(cr: &mut [Crf; 8], v: u32) {
    for (i, f) in cr.iter_mut().enumerate() {
        let n = v >> (28 - 4 * i);
        *f = Crf {
            lt: (n >> 3 & 1) as u8,
            gt: (n >> 2 & 1) as u8,
            eq: (n >> 1 & 1) as u8,
            so: (n & 1) as u8,
        };
    }
}

pub fn pack_xer(x: &Xer) -> u32 {
    (x.so as u32) << 31 | (x.ov as u32) << 30 | (x.ca as u32) << 29 | (x.bc as u32 & 0x7F)
}

pub fn unpack_xer(v: u32) -> Xer {
    Xer {
        so: (v >> 31 & 1) as u8,
        ov: (v >> 30 & 1) as u8,
        ca: (v >> 29 & 1) as u8,
        bc: (v & 0x7F) as u8,
    }
}
