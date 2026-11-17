#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Hexagonal dependency rules (SPEC §4.2), checked on every `cargo test`.

use std::collections::BTreeSet;
use std::process::Command;

use serde_json::Value;

const CORE: &str = "rackend-core";
const BINARY: &str = "rackend";

/// Direct dependencies `core` may have (SPEC §4.2 allowlist).
const CORE_ALLOWED: &[&str] = &[
    "serde",
    "thiserror",
    "tracing",
    "uuid",
    "time",
    "argon2",
    "sha2",
    "hmac",
    "rand",
    "subtle",
    "base64",
    "jsonwebtoken",
    "rsa",
    "p256",
    "yrs",
    "webauthn-rs",
    "tokio",
    // ADR 0007
    "serde_json",
    "regex",
];

/// Crates that must never appear anywhere in `core`'s dependency tree.
const CORE_BANNED: &[&str] = &[
    "rusqlite",
    "libsqlite3-sys",
    "axum",
    "hyper",
    "tower",
    "tower-http",
    "object_store",
    "reqwest",
    "lettre",
    "image",
    "sysinfo",
    "mio",
    "openidconnect",
];

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn metadata() -> Value {
    let out = Command::new(cargo())
        .current_dir(workspace_root())
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// (name, normal dependencies as JSON) of every workspace member.
fn members() -> Vec<(String, Vec<Value>)> {
    metadata()["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let deps = p["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| d["kind"].is_null()) // normal (not dev/build)
                .cloned()
                .collect();
            (p["name"].as_str().unwrap().to_owned(), deps)
        })
        .collect()
}

fn workspace_deps(deps: &[Value]) -> BTreeSet<String> {
    deps.iter()
        .filter(|d| d["path"].is_string())
        .map(|d| d["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn core_has_no_workspace_or_io_dependencies() {
    let members = members();
    let (_, deps) = members.iter().find(|(n, _)| n == CORE).unwrap();
    assert!(
        workspace_deps(deps).is_empty(),
        "core must not depend on other workspace crates"
    );
    for d in deps {
        let name = d["name"].as_str().unwrap();
        assert!(
            CORE_ALLOWED.contains(&name),
            "core may not depend on {name} (SPEC §4.2); add an ADR and update the allowlist"
        );
        if name == "tokio" {
            assert_eq!(
                d["uses_default_features"], false,
                "core: tokio without default features"
            );
            let feats: Vec<&str> = d["features"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f.as_str().unwrap())
                .collect();
            assert_eq!(feats, vec!["sync"], "core: tokio sync primitives only");
        }
    }

    let out = Command::new(cargo())
        .current_dir(workspace_root())
        .args([
            "tree",
            "-p",
            CORE,
            "-e",
            "normal",
            "--prefix",
            "none",
            "-f",
            "{p}",
            "--offline",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = String::from_utf8_lossy(&out.stdout);
    for line in tree.lines() {
        let name = line.split_whitespace().next().unwrap_or_default();
        assert!(
            !CORE_BANNED.contains(&name),
            "core pulls in {name} transitively (SPEC §4.2)"
        );
    }
}

#[test]
fn adapters_depend_only_on_core() {
    for (name, deps) in members() {
        if name == CORE || name == BINARY {
            continue;
        }
        let ws = workspace_deps(&deps);
        let others: Vec<_> = ws.iter().filter(|d| d.as_str() != CORE).collect();
        assert!(
            others.is_empty(),
            "adapter {name} depends on other adapters: {others:?} (SPEC §4.2)"
        );
    }
}

#[test]
fn only_the_binary_depends_on_adapters_and_it_wires_all_of_them() {
    let members = members();
    let adapters: BTreeSet<String> = members
        .iter()
        .map(|(n, _)| n.clone())
        .filter(|n| n != CORE && n != BINARY)
        .collect();
    let (_, bin_deps) = members.iter().find(|(n, _)| n == BINARY).unwrap();
    let wired = workspace_deps(bin_deps);
    for a in &adapters {
        assert!(wired.contains(a), "the composition root must depend on {a}");
    }
}
