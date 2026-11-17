//! S3 contract tests (SPEC §12.2): the blob contract against real
//! S3-compatible servers in Docker. Run with `mise run test:s3`.
//!
//! - Garage (`dxflrs/garage`) and RustFS (`rustfs/rustfs`) always run.
//! - MinIO no longer publishes anonymous images; set
//!   `RACKEND_TEST_MINIO_IMAGE=<repo:tag>` to include an image you can pull.
#![cfg(feature = "contract-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/contract/blob.rs"]
mod blob;

use std::time::Duration;

use hmac::{KeyInit as _, Mac as _};
use rackend_blob_s3::S3BlobStore;
use rackend_core::ports::{BlobStore, S3Config};
use sha2::{Digest as _, Sha256};
use testcontainers::core::{ExecCommand, IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

const BUCKET: &str = "rackend";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hmac(key: &[u8], msg: &str) -> Vec<u8> {
    let mut m = hmac::Hmac::<Sha256>::new_from_slice(key).unwrap();
    m.update(msg.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// `PUT /<bucket>` signed with SigV4: object_store cannot create buckets.
async fn create_bucket(endpoint: &str, region: &str, ak: &str, sk: &str) {
    let host = endpoint.trim_start_matches("http://");
    let now = time::OffsetDateTime::now_utc();
    let date = format!(
        "{:04}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    );
    let amz = format!(
        "{date}T{:02}{:02}{:02}Z",
        now.hour(),
        now.minute(),
        now.second()
    );
    let ph = hex(&Sha256::digest(b""));
    let canonical = format!(
        "PUT\n/{BUCKET}\n\nhost:{host}\nx-amz-content-sha256:{ph}\nx-amz-date:{amz}\n\nhost;x-amz-content-sha256;x-amz-date\n{ph}"
    );
    let scope = format!("{date}/{region}/s3/aws4_request");
    let sts = format!(
        "AWS4-HMAC-SHA256\n{amz}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let mut k = hmac(format!("AWS4{sk}").as_bytes(), &date);
    for part in [region, "s3", "aws4_request"] {
        k = hmac(&k, part);
    }
    let sig = hex(&hmac(&k, &sts));
    let auth = format!(
        "AWS4-HMAC-SHA256 Credential={ak}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={sig}"
    );
    let res = reqwest::Client::new()
        .put(format!("{endpoint}/{BUCKET}"))
        .header("x-amz-date", amz)
        .header("x-amz-content-sha256", ph)
        .header("authorization", auth)
        .send()
        .await
        .unwrap();
    assert!(
        res.status().is_success(),
        "create bucket: {} {}",
        res.status(),
        res.text().await.unwrap_or_default()
    );
}

fn config(endpoint: &str, region: &str, ak: &str, sk: &str, prefix: &str) -> S3Config {
    S3Config {
        endpoint: endpoint.to_owned(),
        region: region.to_owned(),
        bucket: BUCKET.to_owned(),
        prefix: prefix.to_owned(),
        access_key_id: ak.to_owned(),
        secret_access_key: sk.to_owned(),
        force_path_style: true,
        allow_http: true,
    }
}

/// The contract with and without a key prefix, plus presigned downloads.
async fn check(endpoint: &str, region: &str, ak: &str, sk: &str) {
    let dir = tempfile::tempdir().unwrap();
    for prefix in ["", "tenant/a"] {
        let store = S3BlobStore::new(&config(endpoint, region, ak, sk, prefix)).unwrap();
        blob::run(&store, dir.path()).await;
    }
    let store = S3BlobStore::new(&config(endpoint, region, ak, sk, "")).unwrap();
    store
        .put_bytes("signed/x.txt", b"signed body".to_vec(), "text/plain")
        .await
        .unwrap();
    let url = store
        .presign_get("signed/x.txt", Duration::from_secs(60))
        .await
        .unwrap()
        .expect("S3 signs URLs");
    let body = reqwest::get(&url).await.unwrap().bytes().await.unwrap();
    assert_eq!(&body[..], b"signed body");
    store.delete("signed/x.txt").await.unwrap();
}

async fn exec_ok(c: &ContainerAsync<GenericImage>, cmd: &[&str]) -> String {
    let mut r = c.exec(ExecCommand::new(cmd.iter().copied())).await.unwrap();
    let out = String::from_utf8_lossy(&r.stdout_to_vec().await.unwrap()).into_owned();
    let err = String::from_utf8_lossy(&r.stderr_to_vec().await.unwrap()).into_owned();
    let code = r.exit_code().await.unwrap();
    assert!(
        code.is_none_or(|c| c == 0),
        "{cmd:?} failed ({code:?}): {out} {err}"
    );
    out
}

#[tokio::test]
async fn garage() {
    let config = r#"
metadata_dir = "/var/lib/garage/meta"
data_dir = "/var/lib/garage/data"
db_engine = "sqlite"
replication_factor = 1
rpc_bind_addr = "[::]:3901"
rpc_public_addr = "127.0.0.1:3901"
rpc_secret = "1799bccfd7411eddcf9ebd316bc1f5287ad12a68094e1c6ac6abde7e6feae1ec"

[s3_api]
s3_region = "garage"
api_bind_addr = "[::]:3900"
root_domain = ".s3.garage.localhost"
"#;
    let c = GenericImage::new("dxflrs/garage", "v2.1.0")
        .with_exposed_port(3900.tcp())
        .with_wait_for(WaitFor::seconds(1))
        .with_copy_to("/etc/garage.toml", config.as_bytes().to_vec())
        .start()
        .await
        .unwrap();
    // Wait until the node answers, then give it a layout, a key and a bucket.
    let mut status = String::new();
    for _ in 0..50 {
        let mut r = c
            .exec(ExecCommand::new(["/garage", "status"]))
            .await
            .unwrap();
        status = String::from_utf8_lossy(&r.stdout_to_vec().await.unwrap()).into_owned();
        if r.exit_code().await.unwrap().is_none_or(|c| c == 0) && status.contains("HEALTHY") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let node = status
        .split_whitespace()
        .find(|w| w.len() == 16 && w.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| panic!("no node id in: {status}"))
        .to_owned();
    exec_ok(
        &c,
        &[
            "/garage", "layout", "assign", "-z", "dc1", "-c", "1G", &node,
        ],
    )
    .await;
    exec_ok(&c, &["/garage", "layout", "apply", "--version", "1"]).await;
    let (ak, sk) = (
        "GK0123456789abcdef01234567",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    );
    exec_ok(
        &c,
        &["/garage", "key", "import", "--yes", "-n", "rackend", ak, sk],
    )
    .await;
    exec_ok(&c, &["/garage", "bucket", "create", BUCKET]).await;
    exec_ok(
        &c,
        &[
            "/garage", "bucket", "allow", "--read", "--write", "--owner", BUCKET, "--key", ak,
        ],
    )
    .await;
    let port = c.get_host_port_ipv4(3900).await.unwrap();
    check(&format!("http://127.0.0.1:{port}"), "garage", ak, sk).await;
}

async fn wait_http(endpoint: &str) {
    for _ in 0..100 {
        if reqwest::get(endpoint).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{endpoint} did not come up");
}

#[tokio::test]
async fn rustfs() {
    let (ak, sk) = ("rackendtest", "rackendtestsecret");
    let c = GenericImage::new("rustfs/rustfs", "1.0.0")
        .with_exposed_port(9000.tcp())
        .with_wait_for(WaitFor::seconds(1))
        .with_env_var("RUSTFS_ACCESS_KEY", ak)
        .with_env_var("RUSTFS_SECRET_KEY", sk)
        .start()
        .await
        .unwrap();
    let endpoint = format!(
        "http://127.0.0.1:{}",
        c.get_host_port_ipv4(9000).await.unwrap()
    );
    wait_http(&endpoint).await;
    create_bucket(&endpoint, "us-east-1", ak, sk).await;
    check(&endpoint, "us-east-1", ak, sk).await;
}

#[tokio::test]
async fn minio_if_available() {
    let Ok(image) = std::env::var("RACKEND_TEST_MINIO_IMAGE") else {
        eprintln!("skipped: set RACKEND_TEST_MINIO_IMAGE=<repo:tag> to test MinIO");
        return;
    };
    let (name, tag) = image.rsplit_once(':').unwrap_or((&image, "latest"));
    let (ak, sk) = ("rackendtest", "rackendtestsecret");
    let c = GenericImage::new(name, tag)
        .with_exposed_port(9000.tcp())
        .with_wait_for(WaitFor::seconds(1))
        .with_env_var("MINIO_ROOT_USER", ak)
        .with_env_var("MINIO_ROOT_PASSWORD", sk)
        .with_cmd(["server", "/data"])
        .start()
        .await
        .unwrap();
    let endpoint = format!(
        "http://127.0.0.1:{}",
        c.get_host_port_ipv4(9000).await.unwrap()
    );
    wait_http(&endpoint).await;
    create_bucket(&endpoint, "us-east-1", ak, sk).await;
    check(&endpoint, "us-east-1", ak, sk).await;
}
