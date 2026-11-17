# 0015. M6 broker and email details not fixed by the spec

- Status: accepted
- Date: 2026-11-17
- Spec: §10.6, §11.4, §11.7, §11.10, §20 M6

## Context

M6 adds SMTP email with editable templates, email verification, password reset, email change, one-time codes, magic links and external sign-in providers (generic OpenID Connect, a GitHub preset, bundled OIDC presets) with identity linking and exchange codes. This record covers the choices the spec leaves open.

## Decision

### Ports and adapters

- `Mailer` and `MailerFactory` (core) with `rackend-mail-smtp` on `lettre`: STARTTLS, implicit TLS or plain (local relays), rustls with the Mozilla roots, pooled connections, 15 s timeout. The mailer is rebuilt when the `smtp` settings change; an empty host turns email off.
- `IdpClient` (core) with `rackend-idp-client` on `openidconnect`. Its HTTP client is the `reqwest` already in the tree (object storage, OTLP), with rustls, no redirects, a 10 s timeout and a 1 MiB response limit.
- Tests replace both through `AppOptions` (a capturing mailer, the in-process OpenID provider of `tests/api/mock_idp.rs`).

### Email

- Settings: `smtp` (`host`, `port`, `username`, `password` encrypted like other secrets, `tls`, `from_name`, `from_address`) and `email_templates` (`verification`, `password_reset`, `email_change`, `otp`, `magic_link`, each `subject`, `text`, `html`). Variables are replaced in all three; in `html` the values are HTML-escaped, and line breaks are removed from subjects. An empty field falls back to the bundled default.
- `POST /api/settings/test-smtp {to, smtp?}` sends with the saved settings or with unsaved values merged over them, and saves nothing.
- Email goes out from a background task, so request endpoints answer in the same time whether or not a message is sent. The account lookup and the token write run in one transaction either way.
- Links need `app.public_url` (`public_url_not_configured` otherwise); email endpoints answer `smtp_not_configured` while SMTP is off. Both are instance-wide, so they reveal nothing about accounts.

### Email flows

- Verification and reset links open hosted pages: `/auth/verify`, `/auth/reset`, `/auth/email-change`. Opening a link changes nothing; the page's form POST does, so link scanners in mail clients cannot use it up. Pages are plain HTML forms with `default-src 'none'; style-src 'self'; form-action 'self'` (styles from `/auth/page.css`), `Referrer-Policy: no-referrer` and `Cache-Control: no-store`. They ignore the caller's session: they act on the token alone, so a browser signed in to the admin UI (whose form posts carry `Origin: null` under `no-referrer`) is not stopped by the cookie CSRF check.
- Verification tokens remember the address; a token for an address the account no longer has is refused. Already verified accounts get no mail.
- A password reset checks the new password before using the token, sets it, marks the email verified (the link proved access), and ends every session (`auth.password.reset`).
- An email change sends the link to the new address. Confirming needs the current password when the account has one, sets the address as verified, voids other pending tokens and codes, and ends every session (`auth.email.change`). A taken address gets the same 204 and no mail.
- One-time codes: six digits from the OS random source (rejection sampling), stored as HMAC-SHA-256 keyed by the instance secret over `"{otp_id}:{code}"`. A new code voids the previous one. Five wrong codes void it. Unknown addresses get an `otp_id` that matches nothing. A correct code verifies the email.
- Magic links go to `{redirect_uri}#token=…`; `redirect_uri` must be listed in `security.app_redirect_uris` (exact match) or, in dev mode, be `http://localhost` or `http://127.0.0.1` with any port. A magic link sign-in verifies the email.
- `auth-methods` reports `otp` and `magic_link` as enabled only when the collection allows them and SMTP is configured.

### Rate limits

- Two new buckets in `security.rate_limits`: `email_ip_per_10min` (5) and `email_address_per_hour` (3, keyed by the target address whether or not it has an account). Code and magic link sign-ins count against the login buckets; each code and each link is its own account key.

### Providers

- Stored in `_auth_providers` (client secret encrypted). Admin API: `GET/POST /api/auth-providers`, `PATCH/DELETE /api/auth-providers/{id}` (audited `auth_provider.*`), `POST /api/auth-providers/discover`. Removing a provider keeps the linked identities, so adding it back restores them.
- Issuers must be https; plain http is accepted for loopback hosts (local providers and tests) and in dev mode.
- Presets ship as `crates/core/data/provider-presets.json`: Google, Microsoft Entra ID, Apple, GitLab, Authentik, Pocket ID, Keycloak and Zitadel as pre-filled `oidc` configurations (with `{placeholders}` to replace), and GitHub as `oauth2_preset`.
- ID tokens are accepted with the asymmetric algorithms the provider advertises (RS*, PS*, ES256, ES384, EdDSA; RS256 when it lists none). `none` and HMAC are never accepted. An unknown `kid` fetches the provider's metadata and keys again, once.
- GitHub: authorization code with PKCE S256, then `/user` and `/user/emails`. The primary verified address is used; GitHub's numeric user id is the subject, `https://github.com` the issuer.

### Sign-in flow

- `GET /api/collections/{c}/providers/{name}/start?redirect_uri=…[&link=true][&format=json]` sets an encrypted, HttpOnly, `SameSite=Lax` cookie scoped to the callback path for 10 minutes (state, PKCE verifier, nonce, provider, collection, redirect URI, the user to link) and redirects, or answers `{url}` with `format=json`. `redirect_uri` is required: the hosted login page of the OIDC provider (M7) is the only flow without one.
- The callback always ends at `redirect_uri#code=…` or `redirect_uri#error=<code>`. Without a valid state cookie there is nowhere safe to return to, so it shows an error page. Error codes: `invalid_state`, `access_denied`, `provider_error`, `account_not_found`, `email_in_use`, `identity_in_use`, `server_error`.
- Exchange codes are `_verification_tokens` rows (purpose `exchange`, 60 s, single use). They carry the user, not tokens; the session starts when the code is exchanged.

### Linking

The rules of §11.7.1, with these details:

- Rule 1 matches `(issuer, subject)` within the collection; an identity of another collection is `account_not_found`, and one linked to another user while linking is `identity_in_use`.
- Rule 3 also requires the existing account's own email to be verified. Otherwise whoever registered the address first, without access to the inbox, would share the account with its real owner (pre-registration hijacking).
- Any other account with the provider's email, verified or not, ends in `email_in_use`: the user signs in and links the provider explicitly. A sign-up never takes an address that exists.
- Sign-up sets the email from the claim (verified as the provider says) and copies claims to text and URL fields by the collection's `oidc_claims` mapping, overridden per provider by `claim_mapping`. Files (avatars) are not fetched (§17.6).
- A provider-verified email marks a matching unverified account email as verified on sign-in.
- Unlinking is refused when nothing else signs the user in: another identity, a password (with password sign-in on), a passkey (with passkeys on), or the email with codes or magic links on and SMTP configured.

### Dependencies

- New: `lettre`, `openidconnect` (approved in SPEC §3.3), `http` (types for the openidconnect client). `quoted_printable` (via lettre) adds `0BSD` to the license allowlist.
- `openidconnect` brings older versions of some RustCrypto crates next to the ones webauthn-rs uses (rsa 0.9, sha2 0.10); `cargo deny` reports them as duplicates.

### Tests

- `tests/api/email_auth.rs`: every flow with a capturing mailer, and response equivalence (status, headers, body) of all five request endpoints for an existing and an unknown address.
- `tests/api/broker.rs` against `tests/api/mock_idp.rs`: linking rules, the auto-link attack with an unverified email, state, nonce and redirect checks, PKCE, key rotation.
- `tests/api/keycloak.rs` (feature `keycloak-tests`, `mise run test:broker`): sign-in against Keycloak 26.5.7 in Docker and the unverified-email attack with a real provider.
- The timing test of M2 runs in its own nextest group after the others (`.config/nextest.toml`).

## Consequences

- Providers that sign ID tokens with HMAC (client secret) are not supported.
- The broker trusts `email_verified` from the provider, which is why linking by email stays opt-in per provider.
