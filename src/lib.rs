//! Native Rust implementation of the 55pro `.55pro` format.
//!
//! The crate has no third-party dependencies. It includes the block codec,
//! CRC32 verification, a bounded multithreaded block pipeline, and the optional
//! directory payload layer used by the CLI.

pub mod cli;
pub mod codec;
pub mod crc32;
pub mod error;
pub mod json;
pub mod path_archive;

pub use error::{Pro55Error, Result};

pub const VERSION: &str = "0.6.0";
