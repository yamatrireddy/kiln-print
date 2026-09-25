//! Domain model shared by the engine, providers, persistence and the wire protocol.

pub mod document;
pub mod job;
pub mod printer;

pub use document::*;
pub use job::*;
pub use printer::*;
