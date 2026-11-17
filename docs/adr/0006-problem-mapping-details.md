# 0006. Problem mapping: middleware placement, `request_timeout`, unregistered statuses

- Status: accepted
- Date: 2026-11-17
- Spec: §10.2

## Decision
1. **Placement.** The response-mapping middleware wraps the whole router (the router is mounted as a fallback service under the middleware), not each route. With per-route layers axum adds the `Allow` header of a 405 *after* the middleware ran, so `allowed` could not be filled.
2. **New code `request_timeout` (408).** Header/body read timeouts (§17.2) produce 408; the registry in §10.2.2 had no code for it. Added to the registry, `docs/errors.md` and the spec.
3. **Statuses outside the registry** produced by the framework are rewritten so that `status` always equals a registered code's status: `422` (axum JSON data errors) → `400 bad_request`; other unregistered 4xx → `400 bad_request`; other 5xx (e.g. a `502` from a handler) → `500 internal`.
4. **`detail` of framework rejections.** For 4xx responses whose body is a short `text/plain` message (axum extractor rejections), that text becomes `detail`; it only describes the client's input. Bodies of 5xx responses are never copied.

## Consequences
The error contract test (§10.2.6) covers 404, 405 (+`allowed`/`Allow`), bad JSON, wrong JSON shape, wrong content type, oversized body (+`max_bytes`), panics, internal errors with secret markers, 503 (+`Retry-After`), HEAD, request ids and HTML error pages.
