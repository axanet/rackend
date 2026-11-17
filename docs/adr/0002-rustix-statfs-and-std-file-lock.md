# 0002. `rustix` for network-filesystem detection; std file locking

- Status: accepted
- Date: 2026-11-17
- Spec: §3.3, §5.4 step 3–4

## Context
§5.4 asks to detect NFS/SMB/FUSE data directories via `statfs` magic numbers. `statfs` is a libc call; calling it through `libc` needs `unsafe`, which the workspace forbids. §3.3 lists `fs4` for the exclusive data-dir lock.

## Decision
- Add `rustix` (features `fs`, `std`; Linux-only target dependency of the `rackend` binary) for a safe `statfs`. It is already in the dependency tree through `fs4`/`tempfile`.
- Use `std::fs::File::try_lock` (stable since Rust 1.89) for the exclusive lock. `fs4` stays for `available_space` (free-space check before a pre-upgrade backup, §7.3).

## Consequences
- No `unsafe` code. One new direct dependency, no new crate in the tree.
- FUSE is reported generically ("FUSE, e.g. sshfs"): statfs cannot tell sshfs from other FUSE filesystems. It is a warning only.
