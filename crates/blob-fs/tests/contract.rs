//! The filesystem backend passes the blob contract; keys cannot escape the root.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/contract/blob.rs"]
mod blob;

use rackend_blob_fs::FsBlobStore;
use rackend_core::ports::{BlobError, BlobStore};

#[tokio::test]
async fn fs_backend_passes_the_contract() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsBlobStore::new(dir.path().join("storage"), dir.path().join("tmp"));
    blob::run(&store, dir.path()).await;
    // No temp files are left behind.
    assert_eq!(
        std::fs::read_dir(dir.path().join("tmp")).unwrap().count(),
        0
    );
}

#[tokio::test]
async fn keys_cannot_escape_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsBlobStore::new(dir.path().join("storage"), dir.path().join("tmp"));
    for key in ["../escape", "/abs", "a/../../b", "a//b", ""] {
        let r = store.put_bytes(key, b"x".to_vec(), "text/plain").await;
        assert!(matches!(r, Err(BlobError::InvalidKey(_))), "{key}: {r:?}");
    }
    assert!(!dir.path().join("escape").exists());
}
