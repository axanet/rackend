//! The `BlobStore` contract (SPEC §12.1): every backend must pass this
//! suite. Included by path from each backend's tests.

#![allow(dead_code)]

use std::path::Path;

use rackend_core::ports::{BlobError, BlobStore, ByteRange};

async fn read(store: &dyn BlobStore, key: &str, range: Option<ByteRange>) -> Vec<u8> {
    store
        .get(key, range)
        .await
        .unwrap_or_else(|e| panic!("get {key}: {e}"))
        .read_all(64 * 1024 * 1024)
        .await
        .unwrap()
}

/// Runs every check against `store`; `scratch` is a writable directory.
pub async fn run(store: &dyn BlobStore, scratch: &Path) {
    let p = "contract";
    // put_bytes, head, get
    store
        .put_bytes(
            &format!("{p}/a/one.txt"),
            b"hello world".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    let meta = store
        .head(&format!("{p}/a/one.txt"))
        .await
        .unwrap()
        .expect("head after put");
    assert_eq!(meta.size, 11);
    assert_eq!(
        read(store, &format!("{p}/a/one.txt"), None).await,
        b"hello world"
    );

    // ranges
    let r = ByteRange { start: 6, end: 10 };
    let reader = store.get(&format!("{p}/a/one.txt"), Some(r)).await.unwrap();
    assert_eq!(reader.size, 11, "size is the whole blob");
    assert_eq!(reader.read_all(100).await.unwrap(), b"world");
    assert_eq!(
        read(
            store,
            &format!("{p}/a/one.txt"),
            Some(ByteRange { start: 0, end: 0 })
        )
        .await,
        b"h"
    );

    // overwrite
    store
        .put_bytes(
            &format!("{p}/a/one.txt"),
            b"replaced".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    assert_eq!(
        read(store, &format!("{p}/a/one.txt"), None).await,
        b"replaced"
    );

    // put_file, large enough for multipart uploads on S3 (> 16 MB)
    let big: Vec<u8> = (0..17 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    let file = scratch.join("big.bin");
    std::fs::write(&file, &big).unwrap();
    store
        .put_file(&format!("{p}/b/big.bin"), &file, "application/octet-stream")
        .await
        .unwrap();
    assert_eq!(
        store
            .head(&format!("{p}/b/big.bin"))
            .await
            .unwrap()
            .unwrap()
            .size,
        big.len() as u64
    );
    let got = read(store, &format!("{p}/b/big.bin"), None).await;
    assert!(got == big, "large blob round trip");
    let tail = read(
        store,
        &format!("{p}/b/big.bin"),
        Some(ByteRange {
            start: big.len() as u64 - 3,
            end: big.len() as u64 - 1,
        }),
    )
    .await;
    assert_eq!(tail, &big[big.len() - 3..]);

    // list by prefix
    store
        .put_bytes(&format!("{p}/a/two.txt"), b"2".to_vec(), "text/plain")
        .await
        .unwrap();
    store
        .put_bytes(
            &format!("{p}/_thumbs/a/one.txt/10x10.webp"),
            b"t".to_vec(),
            "image/webp",
        )
        .await
        .unwrap();
    let mut keys: Vec<String> = store
        .list(&format!("{p}/a/"))
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.key)
        .collect();
    keys.sort();
    assert_eq!(keys, [format!("{p}/a/one.txt"), format!("{p}/a/two.txt")]);
    let all = store.list(&format!("{p}/")).await.unwrap();
    assert_eq!(all.len(), 4, "{all:?}");
    assert!(
        all.iter().all(|i| i.meta.last_modified.is_some()),
        "list reports ages"
    );
    assert!(
        all.iter()
            .any(|i| i.key == format!("{p}/b/big.bin") && i.meta.size == big.len() as u64)
    );

    // missing blobs
    assert_eq!(store.head(&format!("{p}/missing")).await.unwrap(), None);
    assert!(matches!(
        store.get(&format!("{p}/missing"), None).await,
        Err(BlobError::NotFound)
    ));

    // delete is idempotent
    store.delete(&format!("{p}/a/one.txt")).await.unwrap();
    store.delete(&format!("{p}/a/one.txt")).await.unwrap();
    assert_eq!(store.head(&format!("{p}/a/one.txt")).await.unwrap(), None);

    // cleanup
    for item in store.list(&format!("{p}/")).await.unwrap() {
        store.delete(&item.key).await.unwrap();
    }
    assert!(store.list(&format!("{p}/")).await.unwrap().is_empty());
}
