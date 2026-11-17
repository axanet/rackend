# 0017. M8 OIDC provider details not fixed by the spec

- Status: accepted
- Date: 2026-11-18
- Spec: §11.8, §17.3, §20 M8

## Context

M8 makes rackend an OpenID provider: discovery, JWKS, the authorization code flow with hosted login and consent, token, userinfo, revocation, RP-initiated logout, clients, JWT access tokens on rackend's own API, key rotation and theming. The acceptance criteria are the OpenID Foundation conformance suite (Basic OP, Config OP) and a sample SPA using a standard library. This record covers what the spec leaves open.

## Decision

### Sessions

- Signing in on a hosted page creates a normal `_sessions` row (the browser session). Its refresh token is the value of the `rk_sso` cookie (HttpOnly, `SameSite=Lax`, `Path=/`), checked without rotating it. Idle timeouts and revocation apply as for any session.
- Every code exchange creates a client session: `client_id` set, `family_id` = the browser session, `scopes` = the granted scopes (new column). Its id is the `sid` of the tokens. Sign-out ends the browser session and its whole family, so every app session started from it ends too.
- Codes record the session they created (`issued_session_id`); reusing a code revokes it.

### Tokens and keys

- JWTs are signed in `core` with the `rsa` and `p256` crates already used for key generation: RS256 (PKCS#1 v1.5 with blinding) and ES256. No JWT library is added; `jsonwebtoken` (approved) is not needed.
- Access tokens are JWTs (`typ: at+jwt`) with `aud = [client_id, "{issuer}/api"]`. rackend's API accepts them when the signature, `iss`, `aud` and `exp` check out and the session `sid` is active (cached like opaque tokens, invalidated on revocation).
- ID tokens use the client's `id_token_alg`. Refresh responses include a new ID token; a refresh may narrow the scope, never widen it (checked before the token is rotated).
- Keys: the newest key whose `activated_at` has passed signs. The daily job creates the next key 7 days before the current one is 90 days old, retires replaced keys and deletes them after `max(longest refresh lifetime, 7 days)`. `POST /api/keys/rotate` replaces the active keys at once (audited `keys.rotate`); the old ones stay in the JWKS until their tokens can have expired. `GET /api/keys` lists them for the admin UI.

### Clients

- `_oauth_clients` adds `dev` (loopback redirect URIs with any port), `id_token_alg` and `require_pkce`. PKCE S256 is always required for public clients. Confidential clients may turn it off: the OpenID conformance suite's OIDCC plans and some server-side libraries do not send it, and the client secret protects their codes. The default is on.
- Secrets are `rk_cs_…` (32 random bytes), shown once, stored as SHA-256; `rotate_secret` issues a new one. Deleting a client ends its sessions.
- The `default` client is created once at startup (public, first party, bound to `users`, `dev` in dev mode). A meta flag remembers it, so deleting it is respected.

### Authorization

- While the user signs in or consents, the request travels as `ar`: the validated request, encrypted with the instance key (ChaCha20-Poly1305), valid 30 minutes. Hosted pages carry it in forms; `/oauth2/authorize?ar=…` resumes it. Nothing is stored server-side for unfinished requests.
- `prompt=login` and `max_age` are satisfied by a sign-in during the same request; `max_age=0` always asks again. `prompt=none` answers `login_required` or `consent_required`. Unknown scopes are ignored; `openid` is required. `request` and `request_uri` answer `request_not_supported` / `request_uri_not_supported`.
- Redirects to the client carry `iss` (RFC 9207). Errors before the client and redirect URI are known show an error page and never redirect.

### Hosted pages

- askama templates (`crates/http/templates`) with strings from `crates/http/i18n/en.json`. `/auth/login` offers the collection's methods: password, passkey (vanilla `/auth/page.js`), one-time code, external providers (continuing the request after the provider callback), sign-up and password reset. Magic links are not offered on the hosted page: they open the app, not the authorization flow.
- CSP: `default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; form-action 'self' <client origin>`. The client origin is needed because browsers apply `form-action` to the redirect chain after the login form.
- Login CSRF: hosted forms reject posts whose `Origin` is not the public URL. Pages ignore API credentials; the browser session is the `rk_sso` cookie.
- Theming: `app.name`, `app.logo` (an image data URL of at most 100 KB, set from a file in the admin UI) and `app.primary_color` (served as `/auth/theme.css`). Both are validated.

### Protocol endpoints

- Token, userinfo and revocation errors are RFC 6749 JSON (`error`, `error_description`) plus the RFC 9457 members; `invalid_client` and `invalid_token` answer 401 with `WWW-Authenticate`. Token responses are `no-store`.
- CORS: the OIDC endpoints (discovery, JWKS, token, userinfo, revoke) allow any origin without credentials. `/api` now honors `security.cors_origins` (with credentials for cookie mode), and in dev mode any `http://localhost:*` origin.

### Conformance

- `scripts/conformance/run.sh` runs the suite (pinned image digest, runner scripts fetched at a pinned commit) with MongoDB and a Caddy TLS front for the suite and rackend (`conformance.test`, `rackend.test`; Java sends no SNI for dotless names). It creates a verified test user and two confidential clients, and logs in through the hosted page with the suite's browser automation.

## Consequences

- External resource servers that only verify signatures accept access tokens until they expire (15 minutes by default); rackend itself rejects them as soon as the session ends.
- Clients that cannot do PKCE must be confidential.
