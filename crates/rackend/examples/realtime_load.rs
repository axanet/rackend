//! Opens many idle realtime WebSocket connections, each with one record
//! subscription, and holds them (SPEC §20 M4: 10k idle connections).
//!
//! ```text
//! cargo run --release -p rackend --example realtime_load -- ws://127.0.0.1:8098/api/ws 10000 30
//! ```
//!
//! The server must allow that many connections from one IP
//! (`realtime.max_connections_per_ip`). Measure the server's RSS while the
//! connections are held; see docs/performance.md.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let url = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:8098/api/ws".into());
    let count: usize = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(10_000);
    let hold = Duration::from_secs(args.get(3).and_then(|n| n.parse().ok()).unwrap_or(30));
    let topic = std::env::var("TOPIC").unwrap_or_else(|_| "records:pings".into());

    let ok = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let parallel: usize = std::env::var("PARALLEL")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(64);
    let handshakes = Arc::new(tokio::sync::Semaphore::new(parallel));
    let start = Instant::now();
    let mut tasks = Vec::with_capacity(count);
    for _ in 0..count {
        let (url, topic, ok, failed, handshakes) = (
            url.clone(),
            topic.clone(),
            ok.clone(),
            failed.clone(),
            handshakes.clone(),
        );
        tasks.push(tokio::spawn(async move {
            let permit = handshakes.acquire_owned().await.unwrap();
            let mut ws = match tokio_tungstenite::connect_async(&url).await {
                Ok((ws, _)) => ws,
                Err(e) => {
                    if failed.fetch_add(1, Ordering::Relaxed) < 3 {
                        println!("connect failed: {e}");
                    }
                    return;
                }
            };
            let _ = ws.next().await; // connect
            let sub = format!(r#"{{"t":"sub","id":1,"topic":"{topic}"}}"#);
            if ws.send(Message::Text(sub.into())).await.is_err() {
                failed.fetch_add(1, Ordering::Relaxed);
                return;
            }
            match ws.next().await {
                Some(Ok(Message::Text(t))) if t.contains("\"ack\"") => {
                    ok.fetch_add(1, Ordering::Relaxed);
                }
                _ => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
            drop(permit);
            // Idle: answer pings until the hold time is over.
            let _ =
                tokio::time::timeout(hold, async { while let Some(Ok(_)) = ws.next().await {} })
                    .await;
            let _ = ws.close(None).await;
        }));
    }
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let (o, f) = (ok.load(Ordering::Relaxed), failed.load(Ordering::Relaxed));
        println!(
            "{:>5.1}s connected {o} failed {f}",
            start.elapsed().as_secs_f64()
        );
        if o + f >= count {
            break;
        }
    }
    println!(
        "holding {} connections for {hold:?}",
        ok.load(Ordering::Relaxed)
    );
    for t in tasks {
        let _ = t.await;
    }
}
