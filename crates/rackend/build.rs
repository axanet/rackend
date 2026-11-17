//! Build metadata for `rackend version` and selection of the admin UI bundle.

use std::path::Path;
use std::process::Command;

fn main() {
    // Git commit: RACKEND_GIT_COMMIT overrides (Docker builds without .git).
    println!("cargo:rerun-if-env-changed=RACKEND_GIT_COMMIT");
    let commit = std::env::var("RACKEND_GIT_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned())
        });
    println!(
        "cargo:rustc-env=RACKEND_GIT_COMMIT={}",
        commit.unwrap_or_else(|| "unknown".into())
    );

    // Build date: SOURCE_DATE_EPOCH for reproducible builds, else now.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let date = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .and_then(|s| time::OffsetDateTime::from_unix_timestamp(s).ok())
        .unwrap_or_else(time::OffsetDateTime::now_utc);
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    println!(
        "cargo:rustc-env=RACKEND_BUILD_DATE={}",
        date.format(&fmt).unwrap_or_else(|_| "unknown".into())
    );

    // Admin UI bundle: admin-ui/build if built, else a placeholder page (SPEC §18.1).
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let ui = Path::new(&manifest).join("../../admin-ui/build");
    println!("cargo:rerun-if-changed={}", ui.display());
    let dir = if ui.join("index.html").is_file() {
        ui.canonicalize().unwrap_or(ui)
    } else {
        let out = Path::new(&std::env::var("OUT_DIR").unwrap_or_default()).join("ui-placeholder");
        let _ = std::fs::create_dir_all(&out);
        // Same tokens as the admin UI and the hosted error page (Visual Identity).
        let _ = std::fs::write(
            out.join("index.html"),
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>rackend admin</title>\
<style>:root{color-scheme:light dark}body{margin:0;min-height:100vh;display:grid;place-items:center;\
padding:24px;background:light-dark(#fafaf9,#0a0b0d);color:light-dark(#0b0c0e,#ededef);\
font:16px/1.6 ui-sans-serif,system-ui,-apple-system,'Segoe UI',sans-serif}\
main{max-width:480px;padding:32px;border:1px dashed light-dark(#cfcfca,#30343c);border-radius:10px}\
h1{margin:0 0 8px;font-size:1.4rem;letter-spacing:-.03em}p{margin:0;color:light-dark(#52555c,#a0a3ab)}\
code{font:13px ui-monospace,'SF Mono',Menlo,monospace}</style></head>\
<body><main><h1>Admin UI not built</h1><p>Run <code>mise run ui:build</code>, then rebuild rackend.</p></main></body></html>\n",
        );
        out
    };
    println!("cargo:rustc-env=RACKEND_UI_DIR={}", dir.display());
}
