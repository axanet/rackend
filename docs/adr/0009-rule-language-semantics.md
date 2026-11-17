# 0009. Rule language: NULL handling and other semantic details

- Status: accepted (NULL handling confirmed by the project owner, 2026-11-17)
- Date: 2026-11-17
- Spec: §9.2, §9.3, §9.5

## Context
§9.3 defines the main semantics but leaves several cases open. The SQL compiler and the in-memory reference evaluator must agree exactly (§9.6), so every case needs one answer.

## Decision
1. **Comparisons never match NULL.** `a = b` is false if either side is NULL; `!=` is its negation. So `owner = @request.auth.id` never matches unowned records for anonymous callers. Only the literals `null` and `''` test for emptiness: `x = null` and `x = ''` both mean "NULL or empty string".
2. **Ordering and `~` are false on NULL**; `!~` is the negation of `~`. Ordering against the `null` literal is a compile error.
3. **Sets.** Plain operators require every (left, right) pair to match (vacuously true for empty sets); `?` operators need at least one pair. A single value is a one-element set.
4. **`~` patterns.** The right-hand value is used as the LIKE pattern as-is if it contains `%`; otherwise it is wrapped in `%…%` with `%`, `_` and `\` escaped (`ESCAPE '\'`). This applies to literals (prepared at compile time) and to field/request values (in SQL). LIKE is ASCII case-insensitive, as in SQLite.
5. **Storage-class comparison.** When one side is a JSON value, column affinity is removed with unary `+` so values compare by SQLite storage class (NULL < numbers < text); otherwise plain column references are used so indexes stay usable. The type checker keeps all other comparisons type-homogeneous.
6. **`@request.auth` and `@request.body` cannot be traversed** (`@request.auth.team.name`) in v1; compare ids instead. `@request.auth.<field>` is typed from the auth system fields and the fields of auth collections.
7. **`:length`** is allowed on multi-valued and JSON fields reached through single relations only; it is 0 for NULL. `len(x)` is the character length of text (0 for NULL).
8. **`collection_name` / `collection_id`** are folded to constants and only allowed on the record itself.
9. **`@collection.<name>` aliases**: every reference to the same name in one expression uses the same row; the expression is true if any row (or, for an empty collection, a row of NULLs) makes it true.
10. **Typed paths** carry both field ids and column names; compiled SQL is rebuilt whenever the schema changes.
11. **Locked view rule** → 404 for non-superusers (no existence leak), like a failing view rule. A create/update by a caller who may not view the result returns `204 No Content` instead of the record.

## Consequences
The equivalence property test (≥ 12,000 rule/record pairs on randomized data, including NULLs, dangling relations, JSON values and multi-valued fields) checks evaluator and SQL against each other.
