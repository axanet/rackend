#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Black-box tests against the spawned `rackend` binary (SPEC §19).

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_rackend");

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Instance {
    child: Child,
    port: u16,
}

impl Instance {
    fn spawn(dir: &Path) -> Self {
        let port = free_port();
        let child = Command::new(BIN)
            .arg("serve")
            .env_clear()
            .env("RACKEND_DATA_DIR", dir)
            .env("RACKEND_HTTP_ADDR", format!("127.0.0.1:{port}"))
            .env("RACKEND_LOG_LEVEL", "info")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self { child, port }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// Waits until /api/health answers 200.
    async fn ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("rackend exited early with {status}: {}", self.output());
            }
            if let Ok(r) = reqwest::get(self.url("/api/health")).await
                && r.status() == 200
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("rackend did not become ready");
    }

    fn output(&mut self) -> String {
        use std::io::Read;
        let mut s = String::new();
        if let Some(mut o) = self.child.stdout.take() {
            let _ = o.read_to_string(&mut s);
        }
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut s);
        }
        s
    }

    fn terminate(mut self) -> (ExitStatus, String) {
        let pid = self.child.id().to_string();
        let ok = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
        assert!(ok.success());
        let status = wait_timeout(&mut self.child, Duration::from_secs(20));
        let out = self.output();
        (status, out)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_timeout(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(Instant::now() < deadline, "process did not exit in time");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `rackend serve` expecting it to exit on its own; returns code + output.
fn run_to_exit(dir: &Path) -> (i32, String) {
    let out = Command::new(BIN)
        .arg("serve")
        .env_clear()
        .env("RACKEND_DATA_DIR", dir)
        .env("RACKEND_HTTP_ADDR", format!("127.0.0.1:{}", free_port()))
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.code().unwrap_or(-1), text)
}

#[tokio::test]
async fn starts_with_zero_config_and_serves_health() {
    let dir = tempfile::tempdir().unwrap();
    let mut inst = Instance::spawn(dir.path());
    inst.ready().await;
    let body: serde_json::Value = reqwest::get(inst.url("/api/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["status"], "ok");

    let r = reqwest::get(inst.url("/api/nope")).await.unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(r.headers()["content-type"], "application/problem+json");

    let hc = Command::new(BIN)
        .arg("healthcheck")
        .env_clear()
        .env("RACKEND_HTTP_ADDR", format!("0.0.0.0:{}", inst.port))
        .status()
        .unwrap();
    assert!(hc.success(), "healthcheck against a running server");

    let (status, out) = inst.terminate();
    assert_eq!(status.code(), Some(0), "{out}");
    assert!(out.contains("\"message\":\"stopped\""), "{out}");
    let wal = std::fs::metadata(dir.path().join("data.db-wal")).map_or(0, |m| m.len());
    assert_eq!(wal, 0, "WAL checkpointed and truncated on shutdown");
}

#[tokio::test]
async fn second_instance_on_the_same_dir_exits_75() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = Instance::spawn(dir.path());
    first.ready().await;
    let (code, out) = run_to_exit(dir.path());
    assert_eq!(code, 75, "{out}");
    assert!(out.contains("another rackend instance is using"), "{out}");
    // The first instance is unaffected.
    assert_eq!(
        reqwest::get(first.url("/api/health"))
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn too_new_schema_exits_78_and_leaves_data_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let mut inst = Instance::spawn(dir.path());
    inst.ready().await;
    let (status, _) = inst.terminate();
    assert!(status.success());

    let db = dir.path().join("data.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE _meta SET value = '999' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    }
    let before = std::fs::read(&db).unwrap();
    let (code, out) = run_to_exit(dir.path());
    assert_eq!(code, 78, "{out}");
    assert!(
        out.contains("database schema v999 is newer than this binary supports (v")
            && out.contains("Downgrading is not supported"),
        "{out}"
    );
    assert_eq!(std::fs::read(&db).unwrap(), before, "data.db unchanged");
}

#[tokio::test]
async fn data_survives_restart_and_tmp_is_wiped() {
    let dir = tempfile::tempdir().unwrap();
    let mut inst = Instance::spawn(dir.path());
    inst.ready().await;
    let (status, _) = inst.terminate();
    assert!(status.success());
    {
        let conn = rusqlite::Connection::open(dir.path().join("data.db")).unwrap();
        conn.execute(
            "INSERT INTO _settings (key, value, is_secret, updated) VALUES ('app', '{\"name\":\"kept\"}', 0, 'x')",
            [],
        )
        .unwrap();
    }
    std::fs::write(dir.path().join("tmp/leftover"), b"x").unwrap();

    let mut inst = Instance::spawn(dir.path());
    inst.ready().await;
    assert!(!dir.path().join("tmp/leftover").exists());
    let (status, _) = inst.terminate();
    assert!(status.success());
    let conn = rusqlite::Connection::open(dir.path().join("data.db")).unwrap();
    let v: String = conn
        .query_row("SELECT value FROM _settings WHERE key = 'app'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(v, "{\"name\":\"kept\"}");
}

#[test]
fn version_prints_build_info() {
    let out = Command::new(BIN).arg("version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    for key in ["rackend ", "commit:", "built:", "sqlite:", "features:"] {
        assert!(text.contains(key), "{text}");
    }
}

#[test]
fn invalid_configuration_exits_1() {
    let out = Command::new(BIN)
        .arg("serve")
        .env_clear()
        .env("RACKEND_SQLITE_SYNCHRONOUS", "OFF")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("RACKEND_SQLITE_SYNCHRONOUS"));
}
