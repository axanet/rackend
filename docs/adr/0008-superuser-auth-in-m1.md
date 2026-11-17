# 0008. Minimal superuser authentication is built in M1, not M2

- Status: accepted (confirmed by the project owner, 2026-11-17)
- Date: 2026-11-17
- Spec: §20 (M1, M2), §5.5, §10.7, §11.5, §11.9

## Context
M1 delivers collection management endpoints (superuser only, §10.4) and a minimal admin UI with a **setup** screen. Both need superusers, the setup flow and sessions, which §20 schedules for M2. Without them, M1's collection API would have to be either unprotected or unusable.

## Decision
Build the superuser slice of M2 in M1:
- `_superusers`, `_sessions` (superuser rows only for now) and `_setup_tokens` tables;
- argon2id password hashing (§11.2) with dummy verification for unknown emails;
- setup token on startup while no superuser exists (regenerated on every restart until used, §5.5 step 4), `RACKEND_SUPERUSER_EMAIL`/`_PASSWORD` for headless setup, `POST /api/setup`;
- superuser password login, opaque access/refresh tokens with rotation and reuse detection (§11.5), logout, cookie mode with the CSRF origin check (§11.6) for the admin UI.

First-boot steps that belong to other features (default `users` auth collection, signing keys, default OIDC client, `instance_id`) stay in M2 and are written as idempotent "ensure" steps, so instances created by an M1 build get them on upgrade.

Rule-enforcement tests for regular users (M1 acceptance) run against core use cases with synthetic auth contexts; HTTP authentication of auth-collection users arrives with M2.

## Consequences
M2 shrinks to auth collections, user sessions, passkeys, rate limits, secrets at rest and signing keys. Superuser passkeys stay in M2.
