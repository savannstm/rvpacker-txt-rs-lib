//!Library providing functions for rvpacker-txt-rs. Not intended for use in other projects, but can be.

mod functions;
pub use functions::{determine_extension, parse_ignore, read_to_string_without_bom};

pub mod json;
pub mod purge;
pub mod read;
pub mod statics;
pub mod types;
pub mod write;
