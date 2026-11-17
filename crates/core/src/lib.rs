//! `rackend-core`: domain model, application use cases and ports.
//!
//! This crate contains no I/O. Everything that talks to the outside world
//! (database, blob storage, mail, HTTP) is behind a trait in [`ports`] and
//! implemented by an adapter crate. See SPEC §4 (hexagonal architecture).

pub mod auth;
pub mod domain;
pub mod error;
pub mod ports;
pub mod realtime;
pub mod rules;
pub mod secrets;
pub mod settings;
pub mod time;
pub mod usecases;

pub use error::{Error, Result};
