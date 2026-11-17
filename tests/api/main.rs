//! API tests against the real composition (SQLite store, core, HTTP).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod audit_fault;
mod broker;
mod common;
mod docs_examples;
mod email_auth;
mod error_contract;
mod files;
mod flows;
#[cfg(feature = "keycloak-tests")]
mod keycloak;
mod mock_idp;
mod observe;
mod openapi;
mod realtime;
mod rule_matrix;
mod secrets;
mod soft_authenticator;
mod users;
