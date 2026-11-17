# 0007. `serde_json` and `regex` in `core`

- Status: accepted
- Date: 2026-11-17
- Spec: §4.2, §8.4

## Context
§4.2 lists the crates `core` may depend on. Two pure-computation needs are missing from that list:
- Record values are JSON (`json` fields, multi-valued fields, request bodies, `@request.body` in rules, audit diffs). The domain needs a JSON value type.
- `text` fields have a `pattern` option (regex) that domain validation must check.

## Decision
Allow `serde_json` and `regex` in `core`. Both do no I/O. The dependency-rule test allowlist includes them.

## Consequences
Domain validation and the reference rule evaluator use `serde_json::Value` directly. `regex` patterns are compiled once per schema load and cached with the collection.
