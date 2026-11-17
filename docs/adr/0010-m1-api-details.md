# 0010. M1 API details not fixed by the spec

- Status: accepted
- Date: 2026-11-17
- Spec: §10.3–§10.7, §16

## Decision
- `GET /api/info` also returns `needs_setup` (true while no superuser exists) so the admin UI can route to the setup screen. It reveals nothing beyond that.
- `POST /api/rules/check {collection, rule, operation}`: `collection` is an id, a name, or an **unsaved definition** (so the editor can check rules against fields that do not exist yet); `operation` is `list|view|create|update|delete|filter`. Success: `{"ok": true}`; failure: `rule_compile_error` with `position`.
- Record create returns `201`; update returns `200`. Both return `204` when the caller may not view the result (ADR 0009).
- A failed `POST /api/batch` returns the failing sub-request's problem (its status and code) with two extension members: `failed_index` and `results` (`rolled_back` / the failing status / `skipped`). Nothing is committed.
- `GET /api/collections` returns all collections in one page by default (`per_page` up to 500).
- Superuser endpoints from §10.7 are implemented in M1 (ADR 0008), including cookie mode for the admin UI. Before `app.public_url` is set, the CSRF origin check accepts the request's own host.
- Settings that hold secrets (`smtp`, `storage`, `email_templates`) are rejected by `PATCH /api/settings` until encryption at rest (M2).
