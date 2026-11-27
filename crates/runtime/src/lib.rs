//! Guest machine for the recompiled game: memory, hardware registers, boot environment and the C ABI
//! the generated code calls (`runtime/include/fepor/ppc.h`).

pub mod abi;
pub mod ai;
pub mod ax;
pub mod boot;
pub mod card;
pub mod cheats;
pub mod context;
pub mod di;
pub mod dsp;
pub mod exi;
pub mod hle;
pub mod hw;
pub mod intr;
pub mod machine;
pub mod patch;
pub mod sched;
pub mod si;
pub mod trace;

pub use context::Context;
pub use machine::{machine, Machine, RAM_SIZE};
