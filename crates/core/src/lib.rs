//! Core library shared by the CLI and GUI: configuration, disc reading, extraction.

pub mod analysis;
pub mod bugreport;
pub mod cheats;
pub mod config;
pub mod disc;
pub mod dol;
pub mod extract;
pub mod image;
pub mod memcard;
pub mod memwatch;
pub mod mods;
pub mod paths;
pub mod settings;
pub mod stats;
pub mod text;
pub mod version;

pub use config::Config;
pub use disc::{open_disc, Disc, DiscError, FileEntry};
