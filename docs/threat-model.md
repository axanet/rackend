# Threat model

This is a short list of the threats rackend is designed against and where each mitigation lives. It covers a single instance run by one operator. Report security issues privately through a GitHub security advisory on `axanet/rackend`, not in public issues.

## Assets

- User data in collections, and who may read or change it (rules).
- Credentials: password hashes, passkeys, sessions and tokens.
- Secrets in the settings (SMTP, S3, provider client secrets) and the signing keys.
- The availability of the instance.

## Token theft

**Threat:** an attacker gets an access or refresh token (XSS in a client app, a leaked log, a stolen device).

**Mitigations:**
- Access tokens live 15 minutes by default; refresh tokens rotate on every use.
- A rotated refresh token used a second time marks the session as stolen: it is revoked and the event is audited (`auth.refresh_reuse`). A 10 second grace window covers clients that retry a request.
- Revocation takes effect on the next request; the session cache is invalidated in the same process.
- Tokens are stored hashed (SHA-256). A database leak does not reveal usable tokens.
- Browser apps can use cookie mode: `HttpOnly`, `SameSite=Lax`, `Secure`, with the refresh cookie scoped to the refresh path and an `Origin` check on unsafe requests (CSRF).
- Log lines never contain passwords, tokens, cookies or secrets: fields with those names are redacted, and a test greps captured logs for real test credentials.
- Users see their sessions and can sign out other devices. Changing a password signs out every other session.
- Protected files need a file token that lives two minutes, so a copied file URL stops working quickly; tokens are signed with a key derived from the secrets key.

## Credential guessing and account enumeration

**Threat:** online password guessing; learning which emails have accounts.

**Mitigations:**
- Rate limits per IP (20 logins per minute, 600 API requests per minute) and per account (10 per 15 minutes, then a lockout that doubles). `X-Forwarded-For` is honored only from trusted proxies, so clients cannot pick their own IP.
- Login with an unknown email runs a dummy Argon2id verification; a statistical test keeps both paths within 10% of each other. Both answer with the same 401.
- Passwords: at least 10 characters by default, at most 256, not on the 10,000 most common passwords list.
- Hidden emails cannot be used in filters or sorts by non-superusers.
- Passkeys (with user verification) can replace passwords; superusers can be limited to passkeys.

## Account takeover via linking

**Threat:** an attacker signs in with an external provider or an email flow and ends up attached to someone else's account.

**Mitigations (M6, designed now):**
- An external identity is linked to an existing account only when the provider reports the email as verified and the account's email is verified, or when the signed-in user links it explicitly.
- Email change needs a token sent to the new address and, for password accounts, the current password.
- Unlinking or deleting the last login method is refused, so nobody locks themselves out or leaves an account reachable only through a provider they no longer control.

## Rule misconfiguration

**Threat:** an operator writes a rule that exposes or lets others modify data.

**Mitigations:**
- New collections are locked: every rule defaults to superuser only.
- Rules are compiled and type-checked when saved; errors show their position. Rules compile to parameterized SQL, so a filter can never inject SQL.
- The admin UI warns when update or delete is public (`""`).
- Auth collections hide `password_hash` always and `email` from non-owners unless `email_visibility` is on. `email_verified` cannot be set by users. Batch requests cannot write auth records.
- Every write is audited in the same transaction.

## Denial of service through expensive requests

**Threat:** filters, sorts or expands that make SQLite work for a long time; huge bodies.

**Mitigations:**
- Rule and filter limits (depth, length, joins) and a query timeout (`limits.rule_timeout_ms`, default 2 s) enforced by an SQLite progress handler.
- Body size limits (`limits.max_json_body_kb`), batch size limit, and at most 64 nesting levels in `json` field values.
- Password hashing runs on a bounded pool so login floods cannot starve other requests.
- The writer queue is bounded; when it is full the API answers 503 with `Retry-After` rather than piling up work.

## File-based XSS

**Threat:** an uploaded HTML or SVG file runs script in the instance's origin.

**Mitigations:**
- The type is sniffed from the bytes, never taken from the client's header or filename extension. Fields can restrict allowed types.
- HTML and SVG are always served as `Content-Disposition: attachment`; every file response has `X-Content-Type-Options: nosniff` and a `Content-Security-Policy` with `sandbox` (PDFs excepted, they cannot run script in our origin).
- Stored filenames are generated (`name_random8.ext`) and storage keys cannot contain `..` or absolute paths.

## Privacy leaks and decompression bombs in uploads

**Threat:** photos reveal where they were taken (EXIF GPS); a small file decodes to gigabytes of pixels.

**Mitigations:**
- JPEG, PNG and WebP uploads are re-encoded without metadata, after applying the EXIF orientation (per field, on by default). A golden test checks that GPS data is gone.
- Image dimensions are read from the header before decoding; images above 50 megapixels are refused. Decoders run with memory limits.
- Thumbnails come only in sizes the field lists (plus 100x100), are generated once per size even under concurrent requests, and run with bounded concurrency.

## Secrets at rest

**Threat:** a copy of `data.db` (a backup in the wrong place) leaks provider secrets or signing keys.

**Mitigations:**
- Secrets are encrypted with XChaCha20-Poly1305. With `RACKEND_ENCRYPTION_KEY` the key never touches the disk; without it the key is in the database, which protects only backups that are stored apart from it.
- A wrong key stops the start instead of silently producing garbage. `RACKEND_RESET_SECRETS=true` discards the secrets on purpose.

## Out of scope

- An attacker with shell access on the host or write access to the data directory.
- Multi-instance deployments (not supported in v1).
- Vulnerabilities in client apps built on rackend, beyond offering cookie mode and short-lived tokens.
