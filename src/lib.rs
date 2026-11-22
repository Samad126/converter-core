//! Rust port of the converter backend. No HTTP: a library plus a CLI.
pub mod config;
pub mod convert;
pub mod data;
pub mod email;
pub mod engines;
pub mod error;
pub mod extract;
pub mod formats;
pub mod media;
pub mod native;
pub mod pdf;
pub mod pdf_cli;
pub mod pipelines;
pub mod progress;
pub mod process;
pub mod queue;
pub mod workspace;
pub mod youtube;

pub use error::{Error, ErrorCode, Result};
pub use config::Config;
pub use convert::{convert_file, ConvertOptions};
