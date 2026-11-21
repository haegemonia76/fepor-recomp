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
