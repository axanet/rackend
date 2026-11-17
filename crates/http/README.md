# rackend-http

The axum HTTP adapter: maps requests to core use cases and results to responses. It owns the RFC 9457 problem-details error format and the response-mapping layer that converts every error (including framework errors, unknown routes and panics) into it, request ids, security headers and serving of the embedded admin UI shell (SPEC §10, §16, §17.2). It depends only on `rackend-core`.
