//! GameCube graphics pipeline front end: the command processor (CP), transform unit (XF) and the
//! raster/TEV registers (BP). It turns the write-gather stream into [`Event`]s with CPU-transformed
//! vertices and a snapshot of the render state; a backend (crates/video) draws them.
//!
//! Register layouts follow the hardware documentation as reflected in Dolphin's VideoCommon headers.

pub mod bp;
pub mod cp;
pub mod parser;
pub mod state;
pub mod tev;
pub mod texture;
pub mod vertex;
pub mod xf;

pub use parser::Gx;
pub use state::{DrawCall, Event, Primitive, RenderState, Sink, Vertex};
