# OIDC SPA example

A single page app that signs in through rackend's OpenID provider with the standard [`oidc-client-ts`](https://github.com/authts/oidc-client-ts) library (authorization code flow with PKCE), using the `default` client that every rackend instance has. After signing in it calls rackend's API with the access token.

1. Start rackend in dev mode, so the `default` client accepts `http://localhost:*` redirect URIs:

   ```sh
   RACKEND_DEV=true RACKEND_PUBLIC_URL=http://localhost:8090 RACKEND_HTTP_ADDR=127.0.0.1:8090 rackend serve
   ```

   Outside dev mode, add `http://localhost:5173/` to the redirect URIs (and sign-out URIs) of the `default` app in Settings → Apps, and the SPA's origin to `security.cors_origins`.

2. Create a user (sign-up on the hosted login page works too).

3. Run the example with [bun](https://bun.sh):

   ```sh
   bun install
   bun run dev
   ```

4. Open <http://localhost:5173/>. `?authority=https://your-rackend` points it at another instance, `?client_id=` at another app.
