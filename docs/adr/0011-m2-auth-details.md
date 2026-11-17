# 0011. M2 auth details not fixed by the spec

- Status: accepted
- Date: 2026-11-17
- Spec: §8.6, §11, §15, §17.4, §20 M2

## Context

M2 adds auth collections, user sessions, passkeys, rate limits, secrets at rest and signing keys. The spec fixes the behavior; this record covers the choices it leaves open and the dependency changes.

## Decision

### Dependencies

- `chacha20poly1305` joins the core allowlist. `SecretBox` (XChaCha20-Poly1305) is pure computation on bytes and belongs next to the code that decides what is secret. Pure Rust, no I/O.
- `rsa` is pinned to `=0.10.0-rc.19`, the version `webauthn-rs` 0.6 (through `crypto-glue`) requires, so the tree has one `rsa`. RUSTSEC-2023-0071 (Marvin) is fixed in the 0.10 series, but a release candidate sorts before `0.10.0`, so `cargo deny` still matches it; `deny.toml` ignores that one id with this reason. Drop the pin and the ignore once `crypto-glue` accepts `0.10.0`.
- The common-password list is the SecLists `10k-most-common.txt` (MIT), embedded at `crates/core/data/common-passwords.txt` and matched case-insensitively. Its MIT license text sits next to it (`common-passwords.LICENSE`) and must ship with release notices, because the list is compiled into the binary.

### Secrets at rest

- New bootstrap variable `RACKEND_RESET_SECRETS=true`: when the configured key cannot decrypt the stored secrets (wrong or missing `RACKEND_ENCRYPTION_KEY`), start anyway, discard the secret settings and signing keys, and audit `secrets.reset`. Without it, startup fails with exit 1 and a message naming both ways out.
- A canary (`_meta.secret_check`) detects a wrong key before any secret is touched.
- Moving from the stored key to `RACKEND_ENCRYPTION_KEY` re-encrypts settings secrets and signing keys in one transaction, clears `_meta.local_secret_key` and audits `secrets.rekey`.
- `smtp`, `storage` and `email_templates` stay rejected by `PATCH /api/settings` until the features that use them (M3, M6) define their fields.

### Signing keys

- RS256 (2048 bit) and ES256 keys are generated in the background after start, because RSA generation is slow on small machines. `kid` is the RFC 7638 thumbprint. Nothing uses them before M8.

### Auth collections

- First boot creates a `users` auth collection: a `name` text field; list, view, update and delete rules `id = @request.auth.id`; create rule `""` (public sign-up). The avatar file field waits for file fields (M3).
- `email` and `email_verified` are system fields. Non-superusers cannot change either: verification and email change get their flows in M6.
- Changing a password needs `old_password` (except for superusers) and signs out every other session of that user. Deleting a user revokes its sessions and deletes its passkeys.
- When `email_visibility` is off, non-superusers may not filter or sort by `email` (400). This stops probing for addresses through filters.
- `POST /api/batch` refuses auth collections. Their writes need password hashing, read-only system fields and session revocation, which the batch path does not do.

### Sessions

- `DELETE /api/collections/{c}/sessions` without `?all=true` is a 400, so a missing query string never signs out every device.
- The refresh cookie path is `/api/collections/{c}/auth-refresh` with `{c}` exactly as the client wrote it in the login URL.

### Passkeys

- User verification is required. Login uses discoverable credentials and drops `mediation`, so clients show a plain "Sign in with a passkey" button rather than autofill UI.
- Ceremony state lives in memory for 5 minutes (at most 10,000 open ceremonies). rackend is a single process, so nothing is shared.
- Passkeys need `app.public_url` (the relying party id is its host); otherwise `409 public_url_not_configured`.
- Removing the last login method (last passkey of a user without a password) is refused.
- `security.superuser_require_passkey = true` turns superuser password login off (403); superusers then sign in with a passkey only.

### Rate limits

- Values live in the settings as `security.rate_limits` (`global_ip_per_min` 600, `login_ip_per_min` 20, `login_account_per_15min` 10); 0 turns a bucket off. A `PATCH` may change one key and keep the others. Counters are in memory.
- The account key is `<collection id>:<lowercased email>` (`_superusers:` for superusers). A locked account answers 429 even for the correct password; a successful login clears its history. Lockouts start at 15 minutes and double up to 64 times that.
- Passkey logins count against the per-IP login bucket; the account is unknown before the assertion is verified.
- `/api/health` is exempt from the global bucket so health checks never fail under load.
- The client IP is the socket peer, or, when the peer is in a trusted network, the right-most `X-Forwarded-For` entry that is not itself trusted. `RACKEND_TRUSTED_PROXIES` wins over `security.trusted_proxies`.

## Consequences

- Operators who lose `RACKEND_ENCRYPTION_KEY` have a documented way back that costs only the secrets.
- Clients that used `/api/batch` for user records must switch to the record endpoints.
- Multi-instance deployments (not supported in v1) would need shared ceremony and rate-limit state.
