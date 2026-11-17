//! API tests against the real composition (SQLite store, core, HTTP).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod audit_fault;
mod common;
mod error_contract;
mod flows;
mod rule_matrix;
mod secrets;
mod soft_authenticator;
mod users;
