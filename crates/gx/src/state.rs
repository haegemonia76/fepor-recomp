//! Events produced by the front end and the render-state snapshot attached to draws.

use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primitive {
    Quads,
    Triangles,
    TriangleStrip,
    TriangleFan,
    Lines,
    LineStrip,
    Points,
}

/// A vertex after the transform unit: clip-space position, the two colour channels and texture
/// coordinates (s, t, q).
#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub pos: [f32; 4],
    pub col: [[f32; 4]; 2],
    pub uv: [[f32; 3]; 8],
}

impl Default for Vertex {
    fn default() -> Self {
        Vertex {
            pos: [0.0, 0.0, 0.0, 1.0],
            col: [[1.0; 4]; 2],
            uv: [[0.0, 0.0, 1.0]; 8],
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TevStage {
    pub color_env: u32,
    pub alpha_env: u32,
    pub texmap: u8,
    pub texcoord: u8,
    pub tex_enable: bool,
    /// 0 colour0, 1 colour1, 5/6 bump alpha, 7 zero.
    pub chan: u8,
    pub konst_color: u8,
    pub konst_alpha: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextureDesc {
    /// Main-memory byte address of the image.
    pub addr: u32,
    pub width: u32,
    pub height: u32,
    pub format: u8,
    pub wrap_s: u8,
    pub wrap_t: u8,
    pub mag_linear: bool,
    pub min_linear: bool,
    /// TLUT TMEM offset and format, for the palettised formats.
    pub tlut_offset: u32,
    pub tlut_format: u8,
}

#[derive(Clone, Debug, Default)]
pub struct RenderState {
    pub num_tev_stages: u32,
    pub num_tex_gens: u32,
    pub num_colors: u32,
    pub cull: u8,
    pub tev: [TevStage; 16],
    /// Swap tables: for each, which input channel (0 R, 1 G, 2 B, 3 A) feeds r, g, b, a.
    pub swaps: [[u8; 4]; 4],
    /// PREV, COLOR0..2 as signed 11-bit R, G, B, A.
    pub tev_regs: [[i32; 4]; 4],
    /// KONST0..3 as 8-bit R, G, B, A.
    pub konst: [[u8; 4]; 4],
    pub alpha_compare: u32,
    pub z_mode: u32,
    pub blend_mode: u32,
    pub constant_alpha: u32,
    pub color_update: bool,
    pub alpha_update: bool,
    /// Scissor rectangle in EFB pixels: x, y, w, h.
    pub scissor: [i32; 4],
    /// Viewport in EFB pixels: x, y, w, h, near, far depth.
    pub viewport: [f32; 6],
    pub textures: [TextureDesc; 8],
}

/// Copy from the embedded framebuffer.
#[derive(Clone, Debug)]
pub struct CopyEfb {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Destination in main memory (byte address) and stride in bytes.
    pub dest: u32,
    pub stride: u32,
    pub to_xfb: bool,
    pub clear: bool,
    pub clear_color: [u8; 4],
    pub clear_z: u32,
    pub half_scale: bool,
    /// Target pixel format as in UPE_Copy bits 3..6 (with the format bit swizzle undone).
    pub format: u8,
    pub intensity: bool,
    pub color_update: bool,
    pub alpha_update: bool,
    pub z_update: bool,
}

pub struct DrawCall {
    pub prim: Primitive,
    pub vertices: Vec<Vertex>,
    pub state: Arc<RenderState>,
}

pub enum Event {
    Draw(DrawCall),
    Copy(CopyEfb),
    /// The pipeline finished everything queued so far (`GX_DRAW_DONE`).
    Finish,
    /// `GX_SET_TOKEN` or token-with-interrupt.
    Token(u16),
}

/// Receives events together with main memory (textures are read from it).
pub trait Sink {
    fn event(&mut self, event: Event, ram: &[u8], tluts: &Tluts);
}

/// Palette memory: TLUT data by TMEM offset.
#[derive(Default)]
pub struct Tluts {
    pub data: std::collections::HashMap<u32, Vec<u8>>,
}
