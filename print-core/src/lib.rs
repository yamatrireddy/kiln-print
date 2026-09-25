//! # kiln-core
//!
//! The platform-independent heart of Kiln Print. This crate owns:
//!
//! * the **domain model** — printers, capabilities, documents, jobs ([`model`]);
//! * the **error model** shared by every layer and exposed to SDK clients ([`error`]);
//! * the **extension interfaces** — [`provider::PrintProvider`],
//!   [`provider::PrinterDiscoveryProvider`], [`renderer::DocumentRenderer`],
//!   [`protocol::PrinterProtocol`] and [`repository::JobRepository`];
//! * the **engine** — job manager, per-printer ordered queues, spooler monitoring
//!   and printer discovery ([`engine`]).
//!
//! Nothing in this crate touches an operating-system printing API. Platform code lives
//! in provider crates (`kiln-provider-windows`, …) that implement [`provider::PrintProvider`].

#![forbid(unsafe_code)]

pub mod engine;
pub mod error;
pub mod events;
pub mod model;
pub mod protocol;
pub mod provider;
pub mod queue;
pub mod renderer;
pub mod repository;

pub use error::{ErrorCode, PrintError, Result};
