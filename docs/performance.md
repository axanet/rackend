# Performance measurements

Measurements behind the quality attributes (SPEC §2). Each entry names the hardware, the build and how to repeat it.

## Realtime: 10,000 idle connections (M4)

**Setup.** Release build (`cargo build --release -p rackend`), mimalloc, logs at `warn`. One collection with a public view rule. The load client (`crates/rackend/examples/realtime_load.rs`) opens WebSocket connections from the same machine, each with one subscription (`records:pings`), and holds them idle. For the run, `realtime.max_connections_per_ip` was raised to 20,000 and the global per-IP request limit turned off (`security.rate_limits.global_ip_per_min: 0`), because every connection comes from 127.0.0.1.

**Hardware.** Apple M6, 12 cores, 24 GB, macOS. The reference hardware in SPEC §2 (4 vCPU Linux, NVMe) was not available; Linux numbers should be added when it is.

| State | Server RSS |
|---|---|
| Idle after start (no connections) | 44.6 MB |
| Ready for the test (settings changed, one collection) | 56.7 MB |
| 10,000 connections open and subscribed | 147.1 MB |
| After those 10,000 disconnected | 147.0 MB |
| After four rounds of 10,000 | 161.7 MB |

- All 10,000 connections opened in about one second with no failures (64 handshakes in parallel).
- About **9 KB per idle subscribed connection**. WebSocket read and write buffers are set to 4 KB (the defaults are 128 KB each, which would cost about 2.5 GB at this scale).
- Memory is not returned to the OS after disconnects: the allocator keeps the pages for reuse. A test checks that the hub forgets every closed connection (`closed_websockets_leave_no_state_behind`), so the slow growth across rounds is allocator fragmentation, not a leak.
- Idle RSS stays under the 50 MB budget.

**Repeat.**

```bash
ulimit -n 65536
RACKEND_DATA_DIR=/tmp/rk RACKEND_HTTP_ADDR=127.0.0.1:8098 ./target/release/rackend serve
# as a superuser: PATCH /api/settings {"realtime":{"max_connections_per_ip":20000},"security":{"rate_limits":{"global_ip_per_min":0}}}
# and create a collection "pings" with view rule ""
cargo run --release -p rackend --example realtime_load -- ws://127.0.0.1:8098/api/ws 10000 30
ps -o rss= -p <server pid>
```

On macOS a single client can open about 16,000 connections before it runs out of ephemeral ports; wait for `TIME_WAIT` sockets to clear between runs.
