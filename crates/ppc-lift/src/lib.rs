//! Gekko to C++ lifter: turns an [`Analysis`](fepor_core::analysis::Analysis) into compilable C++
//! (one function per guest function, grouped into compilation units) plus a dispatch table.

mod emit;
mod ins;

pub use emit::{generate, GenerateError, Options, Stats};
