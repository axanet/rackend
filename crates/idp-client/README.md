# rackend-idp-client

Implements the core `IdpClient` port (SPEC §11.7): OIDC discovery (cached 24 h), authorization URLs with PKCE S256, state and nonce, code exchange, ID token validation with key refresh on an unknown `kid`, and the GitHub OAuth2 preset. See ADR 0015.
