# 0004. Acquire the data-dir lock before wiping `tmp/`

- Status: accepted
- Date: 2026-11-17
- Spec: §5.4 steps 2–3

## Context
§5.4 lists "wipe `tmp/`" (step 2) before "acquire the exclusive lock" (step 3). If a second instance is started on a directory that is in use, it would delete the running instance's upload staging files before discovering the lock.

## Decision
Order: create the data dir → check it is writable → acquire the lock (exit 75 on failure) → wipe `tmp/`. The spec note "Startup Sequence" is updated accordingly (changelog 0.3).

## Consequences
A second instance never touches the first one's files. No other behaviour changes.
