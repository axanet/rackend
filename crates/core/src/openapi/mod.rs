//! OpenAPI document of an instance (SPEC §10.12): every client-facing
//! endpoint, with component schemas derived from the live collections.
//!
//! The document is built as OpenAPI 3.1 and converted to 3.0.3 on request.
//! Keys are sorted (serde_json maps), so the output is deterministic.

mod yaml;

use serde_json::{Map, Value, json};

pub use yaml::{to_json, to_yaml};

use crate::domain::field::FieldKind;
use crate::domain::{Collection, CollectionType, Field, RuleOp, Schema};

/// The OpenAPI version to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Version {
    /// OpenAPI 3.1.0 (default).
    #[default]
    V31,
    /// OpenAPI 3.0.3, for generators with incomplete 3.1 support.
    V30,
}

impl Version {
    /// `3.1` or `3.0`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "3.1" | "3.1.0" => Some(Self::V31),
            "3.0" | "3.0.3" => Some(Self::V30),
            _ => None,
        }
    }
}

/// An external sign-in provider of an auth collection.
#[derive(Debug, Clone)]
pub struct ProviderRef {
    /// Collection id.
    pub collection_id: String,
    /// Provider name.
    pub name: String,
}

/// What the document describes.
#[derive(Debug, Clone)]
pub struct DocInput<'a> {
    /// The collections.
    pub schema: &'a Schema,
    /// Output version.
    pub version: Version,
    /// Superuser and admin endpoints too (`?include=admin`).
    pub include_admin: bool,
    /// Also operations only superusers may call (locked rules). Off for the
    /// public document, so it does not describe what clients cannot use.
    pub superuser_view: bool,
    /// `app.name`.
    pub app_name: String,
    /// Binary version.
    pub app_version: String,
    /// `app.public_url`, the server URL.
    pub public_url: Option<String>,
    /// Enabled providers.
    pub providers: Vec<ProviderRef>,
}

/// `posts` → `Posts`, `user_profiles` → `UserProfiles`.
pub fn pascal(name: &str) -> String {
    name.split(['_', '-'])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut cs = p.chars();
            cs.next()
                .map(|f| f.to_ascii_uppercase().to_string() + cs.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn r(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

/// A 3.1 nullable schema (converted for 3.0 at the end).
fn nullable(s: Value) -> Value {
    match s.get("type").and_then(Value::as_str) {
        Some(t) => {
            let mut s = s.clone();
            s["type"] = json!([t, "null"]);
            s
        }
        None => json!({ "anyOf": [s, { "type": "null" }] }),
    }
}

/// The schema of one value of a field (no nullability).
fn value_schema(f: &Field, schema: &Schema, owner: &str) -> Value {
    let one = match &f.kind {
        FieldKind::Text {
            min, max, pattern, ..
        } => {
            let mut s = json!({ "type": "string" });
            if *max > 0 {
                s["maxLength"] = json!(max);
            }
            if let Some(m) = min {
                s["minLength"] = json!(m);
            }
            if let Some(p) = pattern {
                s["pattern"] = json!(p.source);
            }
            s
        }
        FieldKind::Email { .. } => json!({ "type": "string", "format": "email" }),
        FieldKind::Url { .. } => json!({ "type": "string", "format": "uri" }),
        FieldKind::Number {
            min,
            max,
            integer_only,
            ..
        } => {
            let mut s = json!({ "type": if *integer_only { "integer" } else { "number" } });
            if let Some(m) = min {
                s["minimum"] = json!(m);
            }
            if let Some(m) = max {
                s["maximum"] = json!(m);
            }
            s
        }
        FieldKind::Bool => json!({ "type": "boolean" }),
        FieldKind::Date { .. } | FieldKind::Autodate { .. } => {
            json!({ "type": "string", "format": "date-time" })
        }
        // Named, so generators make one enum type per field (inline enums
        // of request parameters and models would clash).
        FieldKind::Select { .. } => r(&enum_name(owner, f)),
        FieldKind::Json { .. } => json!({}),
        FieldKind::Relation { collection_id, .. } => {
            let target = schema
                .by_id(*collection_id)
                .map(|c| format!(" Id of a `{}` record.", c.name))
                .unwrap_or_default();
            json!({ "type": "string", "format": "uuid", "description": target.trim() })
        }
        FieldKind::File { .. } => {
            json!({ "type": "string", "description": "Stored filename; download it from /api/files/{collection}/{id}/{filename}." })
        }
    };
    if f.is_multi() {
        json!({ "type": "array", "items": one })
    } else {
        one
    }
}

/// The component name of a select field's values: `PostsStatus`.
fn enum_name(owner: &str, f: &Field) -> String {
    format!("{owner}{}", pascal(&f.name))
}

/// The field in a record response.
fn record_field(f: &Field, schema: &Schema, owner: &str) -> Value {
    let v = value_schema(f, schema, owner);
    match &f.kind {
        FieldKind::Bool | FieldKind::Json { .. } => v,
        _ => nullable(v),
    }
}

fn is_auth(c: &Collection) -> bool {
    c.kind == CollectionType::Auth
}

fn problem_schemas(schemas: &mut Map<String, Value>) {
    schemas.insert(
        "FieldError".into(),
        json!({
            "type": "object",
            "required": ["code", "message"],
            "properties": {
                "code": { "type": "string", "description": "Stable snake_case code, e.g. `too_long`." },
                "message": { "type": "string" },
                "params": { "type": "object", "additionalProperties": true },
            },
        }),
    );
    schemas.insert(
        "Problem".into(),
        json!({
            "type": "object",
            "description": "RFC 9457 problem details. Rely on `code`; see docs/errors.md.",
            "required": ["type", "title", "status", "code", "request_id"],
            "properties": {
                "type": { "type": "string", "format": "uri" },
                "title": { "type": "string" },
                "status": { "type": "integer" },
                "detail": { "type": "string" },
                "instance": { "type": "string" },
                "code": { "type": "string" },
                "request_id": { "type": "string" },
                "errors": { "type": "object", "additionalProperties": { "type": "array", "items": r("FieldError") } },
                "position": { "type": "object", "properties": { "line": { "type": "integer" }, "column": { "type": "integer" } } },
                "retry_after_s": { "type": "integer" },
                "max_bytes": { "type": "integer" },
                "allowed": { "type": "array", "items": { "type": "string" } },
            },
            "additionalProperties": true,
        }),
    );
}

struct Builder<'a> {
    input: &'a DocInput<'a>,
    paths: Map<String, Value>,
    schemas: Map<String, Value>,
}

/// One operation.
struct Op {
    id: String,
    summary: String,
    tag: String,
    params: Vec<Value>,
    body: Option<Value>,
    ok: (u16, Value),
    extra_ok: Vec<(u16, Value)>,
}

impl Op {
    fn new(id: impl Into<String>, summary: impl Into<String>, tag: &str) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            tag: tag.to_owned(),
            params: Vec::new(),
            body: None,
            ok: (204, json!({ "description": "Done." })),
            extra_ok: Vec::new(),
        }
    }

    fn json_body(mut self, schema: Value, required: bool) -> Self {
        self.body = Some(json!({
            "required": required,
            "content": { "application/json": { "schema": schema } },
        }));
        self
    }

    fn returns(mut self, status: u16, description: &str, schema: Value) -> Self {
        self.ok = (
            status,
            json!({ "description": description, "content": { "application/json": { "schema": schema } } }),
        );
        self
    }

    fn no_content(mut self, description: &str) -> Self {
        self.ok = (204, json!({ "description": description }));
        self
    }

    fn also(mut self, status: u16, description: &str) -> Self {
        self.extra_ok
            .push((status, json!({ "description": description })));
        self
    }

    fn param(mut self, p: Value) -> Self {
        self.params.push(p);
        self
    }

    fn build(self) -> Value {
        let mut responses = Map::new();
        responses.insert(self.ok.0.to_string(), self.ok.1);
        for (s, v) in self.extra_ok {
            responses.insert(s.to_string(), v);
        }
        responses.insert(
            "default".into(),
            json!({ "$ref": "#/components/responses/Problem" }),
        );
        let mut op = json!({
            "operationId": self.id,
            "summary": self.summary,
            "tags": [self.tag],
            "responses": responses,
        });
        if !self.params.is_empty() {
            op["parameters"] = Value::Array(self.params);
        }
        if let Some(b) = self.body {
            op["requestBody"] = b;
        }
        op
    }
}

fn query(name: &str, schema: Value, description: &str) -> Value {
    json!({ "name": name, "in": "query", "required": false, "schema": schema, "description": description })
}

fn path_param(name: &str, description: &str) -> Value {
    json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string" }, "description": description })
}

fn uuid_param(name: &str, description: &str) -> Value {
    json!({ "name": name, "in": "path", "required": true, "schema": { "type": "string", "format": "uuid" }, "description": description })
}

fn obj() -> Value {
    json!({ "type": "object", "additionalProperties": true })
}

fn email_body() -> Value {
    json!({ "type": "object", "required": ["email"], "properties": { "email": { "type": "string", "format": "email" } } })
}

impl Builder<'_> {
    fn add(&mut self, path: &str, method: &str, op: Op) {
        let item = self
            .paths
            .entry(path.to_owned())
            .or_insert_with(|| json!({}));
        item[method] = op.build();
    }

    fn allowed(&self, c: &Collection, op: RuleOp) -> bool {
        self.input.superuser_view || c.rules.get(op).is_some()
    }

    fn collection(&mut self, c: &Collection) {
        let schema = self.input.schema;
        let name = pascal(&c.name);
        let auth = is_auth(c);
        let fields: Vec<&Field> = c.fields.iter().collect();

        // Record.
        let mut props = Map::new();
        props.insert("id".into(), json!({ "type": "string", "format": "uuid" }));
        props.insert(
            "collection_id".into(),
            json!({ "type": "string", "format": "uuid" }),
        );
        props.insert("collection_name".into(), json!({ "type": "string" }));
        props.insert(
            "created".into(),
            json!({ "type": "string", "format": "date-time" }),
        );
        props.insert(
            "updated".into(),
            json!({ "type": "string", "format": "date-time" }),
        );
        let mut required: Vec<String> = [
            "id",
            "collection_id",
            "collection_name",
            "created",
            "updated",
        ]
        .map(String::from)
        .into();
        for f in &fields {
            props.insert(f.name.clone(), record_field(f, schema, &name));
            if let FieldKind::Select { values, .. } = &f.kind {
                self.schemas.insert(
                    enum_name(&name, f),
                    json!({ "type": "string", "enum": values }),
                );
            }
            // A hidden email is left out for other users.
            if !(auth && f.name == "email") {
                required.push(f.name.clone());
            }
        }
        let relations: Vec<&&Field> = fields
            .iter()
            .filter(|f| matches!(f.kind, FieldKind::Relation { .. }))
            .collect();
        let mut expand = Map::new();
        for f in relations {
            if let FieldKind::Relation { collection_id, .. } = f.kind
                && let Some(target) = schema.by_id(collection_id)
            {
                let rec = r(&format!("{}Record", pascal(&target.name)));
                expand.insert(
                    f.name.clone(),
                    if f.is_multi() {
                        json!({ "type": "array", "items": rec })
                    } else {
                        rec
                    },
                );
            }
        }
        self.schemas.insert(
            format!("{name}Expand"),
            json!({
                "type": "object",
                "description": "Related records requested with `expand`. Back-relations (`<collection>_via_<field>`) are arrays.",
                "properties": expand,
                "additionalProperties": true,
            }),
        );
        props.insert("expand".into(), r(&format!("{name}Expand")));
        self.schemas.insert(
            format!("{name}Record"),
            json!({ "type": "object", "required": required, "properties": props, "additionalProperties": true }),
        );

        // Create and update.
        let writable: Vec<&&Field> = fields
            .iter()
            .filter(|f| !matches!(f.kind, FieldKind::Autodate { .. }))
            .collect();
        let mut create = Map::new();
        let mut update = Map::new();
        let mut create_required = Vec::new();
        let mut modifiers = Vec::new();
        create.insert("id".into(), json!({ "type": "string", "format": "uuid", "description": "Optional; generated when absent." }));
        for f in &writable {
            let mut s = value_schema(f, schema, &name);
            if f.name == "email_verified" {
                s["description"] = json!("Superusers only.");
            }
            create.insert(f.name.clone(), s.clone());
            update.insert(f.name.clone(), s.clone());
            // Modifier keys are documented, not modelled: generators turn
            // `tags+`, `+tags` and `tags-` into the same identifier.
            if f.is_multi() {
                modifiers.push(format!(
                    "`{n}+` appends, `+{n}` prepends, `{n}-` removes (a value or a list)",
                    n = f.name
                ));
            } else if let FieldKind::Number { .. } = f.kind {
                modifiers.push(format!(
                    "`{n}+` adds to and `{n}-` subtracts from `{n}`",
                    n = f.name
                ));
            }
            let req = match &f.kind {
                FieldKind::Text { required, .. }
                | FieldKind::Email { required, .. }
                | FieldKind::Url { required, .. }
                | FieldKind::Number { required, .. }
                | FieldKind::Date { required, .. }
                | FieldKind::Select { required, .. }
                | FieldKind::Relation { required, .. }
                | FieldKind::File { required, .. } => *required,
                _ => false,
            };
            if req && !matches!(f.kind, FieldKind::File { .. }) {
                create_required.push(f.name.clone());
            }
        }
        if auth {
            create.insert(
                "password".into(),
                json!({ "type": "string", "format": "password", "writeOnly": true }),
            );
            update.insert(
                "password".into(),
                json!({ "type": "string", "format": "password", "writeOnly": true }),
            );
            update.insert(
                "old_password".into(),
                json!({ "type": "string", "format": "password", "writeOnly": true, "description": "Required to change your own password." }),
            );
        }
        let mut create_schema = json!({ "type": "object", "properties": create });
        if !create_required.is_empty() {
            create_schema["required"] = json!(create_required);
        }
        self.schemas.insert(format!("{name}Create"), create_schema);
        let mut update_schema = json!({ "type": "object", "properties": update });
        if !modifiers.is_empty() {
            update_schema["description"] = json!(format!(
                "Fields to change. Modifiers, applied atomically: {}.",
                modifiers.join("; ")
            ));
            update_schema["additionalProperties"] = json!(true);
        }
        self.schemas.insert(format!("{name}Update"), update_schema);

        // Multipart bodies when the collection takes files.
        let has_files = writable
            .iter()
            .any(|f| matches!(f.kind, FieldKind::File { .. }));
        if has_files {
            let mut mp = Map::new();
            for f in &writable {
                let s = match (&f.kind, f.is_multi()) {
                    (FieldKind::File { .. }, false) => {
                        json!({ "type": "string", "format": "binary" })
                    }
                    (FieldKind::File { .. }, true) => {
                        json!({ "type": "array", "items": { "type": "string", "format": "binary" } })
                    }
                    (FieldKind::Json { .. }, _) => {
                        json!({ "type": "string", "description": "JSON-encoded value." })
                    }
                    _ => value_schema(f, schema, &name),
                };
                mp.insert(f.name.clone(), s);
            }
            if auth {
                mp.insert(
                    "password".into(),
                    json!({ "type": "string", "format": "password" }),
                );
            }
            mp.insert(
                "@json".into(),
                json!({ "type": "string", "description": "Optional JSON object with more field values (including modifiers)." }),
            );
            self.schemas.insert(
                format!("{name}Multipart"),
                json!({ "type": "object", "properties": mp }),
            );
        }

        self.schemas.insert(
            format!("{name}List"),
            json!({
                "type": "object",
                "required": ["page", "per_page", "items"],
                "properties": {
                    "page": { "type": "integer" },
                    "per_page": { "type": "integer" },
                    "total_items": nullable(json!({ "type": "integer" })),
                    "total_pages": nullable(json!({ "type": "integer" })),
                    "items": { "type": "array", "items": r(&format!("{name}Record")) },
                },
            }),
        );

        let base = format!("/api/collections/{}/records", c.name);
        let item = format!("{base}/{{id}}");
        let tag = c.name.as_str();
        let id = || uuid_param("id", "Record id.");
        let expand_q = || {
            query(
                "expand",
                json!({ "type": "string" }),
                "Relations to include, e.g. `author,comments_via_post.author` (3 levels at most).",
            )
        };
        let fields_q = || {
            query(
                "fields",
                json!({ "type": "string" }),
                "Fields to return, e.g. `id,title,expand.author.name`.",
            )
        };
        // With file fields, only the multipart form is described: some
        // generators (Python) always pick a JSON media type when one is
        // listed. The server takes JSON bodies (`{name}Create`/`Update`) too.
        let body = |kind: &str| {
            if has_files {
                json!({
                    "required": true,
                    "description": format!("Fields as form parts, files as file parts. A JSON body (`{name}{kind}`) works as well."),
                    "content": { "multipart/form-data": { "schema": r(&format!("{name}Multipart")) } },
                })
            } else {
                json!({ "required": true, "content": { "application/json": { "schema": r(&format!("{name}{kind}")) } } })
            }
        };
        let rec = r(&format!("{name}Record"));

        if self.allowed(c, RuleOp::List) {
            let op = Op::new(format!("{}_list", c.name), format!("List {} records", c.name), tag)
                .param(query("page", json!({ "type": "integer", "minimum": 1 }), "1-based page."))
                .param(query("per_page", json!({ "type": "integer", "minimum": 1, "maximum": 500 }), "Page size (default 30, at most 500)."))
                .param(query("sort", json!({ "type": "string" }), "Comma-separated fields, `-` for descending, `@random`, e.g. `-created,title`."))
                .param(query("filter", json!({ "type": "string" }), "Rule-language expression, e.g. `status = 'published' && views > 10`. See docs/guide/rules.md."))
                .param(expand_q())
                .param(fields_q())
                .param(query("skip_total", json!({ "type": "boolean" }), "Skip counting; `total_items` and `total_pages` are null."))
                .returns(200, "A page of records.", r(&format!("{name}List")));
            self.add(&base, "get", op);
        }
        if self.allowed(c, RuleOp::Create) {
            let mut op = Op::new(
                format!("{}_create", c.name),
                format!("Create a {} record", c.name),
                tag,
            )
            .returns(201, "The new record.", rec.clone())
            .also(204, "Created, but the caller may not view it.");
            op.body = Some(body("Create"));
            self.add(&base, "post", op);
        }
        if self.allowed(c, RuleOp::View) {
            let op = Op::new(
                format!("{}_get", c.name),
                format!("Get a {} record", c.name),
                tag,
            )
            .param(id())
            .param(expand_q())
            .param(fields_q())
            .returns(200, "The record.", rec.clone());
            self.add(&item, "get", op);
        }
        if self.allowed(c, RuleOp::Update) {
            let mut op = Op::new(format!("{}_update", c.name), format!("Update a {} record", c.name), tag)
                .param(id())
                .param(json!({ "name": "If-Match", "in": "header", "required": false, "schema": { "type": "string" }, "description": "The `updated` value you read; fails with 412 if the record changed since." }))
                .returns(200, "The updated record.", rec)
                .also(204, "Updated, but the caller may not view it.");
            op.body = Some(body("Update"));
            self.add(&item, "patch", op);
        }
        if self.allowed(c, RuleOp::Delete) {
            let op = Op::new(format!("{}_delete", c.name), format!("Delete a {} record", c.name), tag)
                .param(id())
                .param(json!({ "name": "If-Match", "in": "header", "required": false, "schema": { "type": "string" } }))
                .no_content("Deleted.");
            self.add(&item, "delete", op);
        }
        if auth {
            self.auth(c);
        }
    }

    fn auth(&mut self, c: &Collection) {
        let Some(o) = c.auth() else { return };
        let n = c.name.as_str();
        let name = pascal(n);
        let base = format!("/api/collections/{n}");
        let tokens = format!("{name}AuthResponse");
        self.schemas.insert(
            tokens.clone(),
            json!({
                "type": "object",
                "required": ["token_type", "expires_in", "record"],
                "properties": {
                    "access_token": { "type": "string" },
                    "refresh_token": { "type": "string" },
                    "token_type": { "type": "string", "description": "`Bearer`, or `Cookie` in cookie mode (header `X-Auth-Mode: cookie`)." },
                    "expires_in": { "type": "integer" },
                    "record": r(&format!("{name}Record")),
                },
            }),
        );
        let tok = r(&tokens);
        let id = |s: &str| format!("{n}_{s}");
        self.add(
            &format!("{base}/auth-methods"),
            "get",
            Op::new(id("auth_methods"), "Enabled sign-in methods", n).returns(
                200,
                "Methods and providers.",
                r("AuthMethods"),
            ),
        );
        if o.methods.password {
            self.add(
                &format!("{base}/auth-with-password"),
                "post",
                Op::new(id("auth_with_password"), "Sign in with email and password", n)
                    .json_body(json!({ "type": "object", "required": ["email", "password"], "properties": { "email": { "type": "string" }, "password": { "type": "string", "format": "password" } } }), true)
                    .returns(200, "Tokens and the record.", tok.clone()),
            );
        }
        self.add(
            &format!("{base}/auth-refresh"),
            "post",
            Op::new(id("auth_refresh"), "Rotate the refresh token", n)
                .json_body(json!({ "type": "object", "properties": { "refresh_token": { "type": "string" } } }), false)
                .returns(200, "New tokens.", tok.clone()),
        );
        self.add(
            &format!("{base}/logout"),
            "post",
            Op::new(id("logout"), "End the current session", n).no_content("Signed out."),
        );
        self.add(
            &format!("{base}/sessions"),
            "get",
            Op::new(id("sessions_list"), "Your sessions (devices)", n).returns(200, "Sessions.", json!({ "type": "object", "properties": { "items": { "type": "array", "items": obj() } } })),
        );
        self.add(
            &format!("{base}/sessions"),
            "delete",
            Op::new(
                id("sessions_revoke_others"),
                "Sign out every other device",
                n,
            )
            .param(query("all", json!({ "type": "boolean" }), "Must be true."))
            .no_content("Done."),
        );
        self.add(
            &format!("{base}/sessions/{{id}}"),
            "delete",
            Op::new(id("session_revoke"), "Revoke one session", n)
                .param(path_param("id", "Session id."))
                .no_content("Revoked."),
        );
        if o.methods.passkey {
            self.add(&format!("{base}/passkeys"), "get", Op::new(id("passkeys_list"), "Your passkeys", n).returns(200, "Passkeys.", json!({ "type": "object", "properties": { "items": { "type": "array", "items": obj() } } })));
            self.add(
                &format!("{base}/passkeys/{{id}}"),
                "delete",
                Op::new(id("passkey_delete"), "Remove a passkey", n)
                    .param(path_param("id", "Passkey id."))
                    .no_content("Removed."),
            );
            for (path, op_id, summary) in [
                (
                    "register/start",
                    "passkey_register_start",
                    "Start adding a passkey (WebAuthn options)",
                ),
                (
                    "register/finish",
                    "passkey_register_finish",
                    "Finish adding a passkey",
                ),
                (
                    "login/start",
                    "passkey_login_start",
                    "Start a passkey sign-in (WebAuthn options)",
                ),
            ] {
                self.add(
                    &format!("{base}/passkeys/{path}"),
                    "post",
                    Op::new(id(op_id), summary, n)
                        .json_body(obj(), false)
                        .returns(200, "WebAuthn JSON.", obj()),
                );
            }
            self.add(
                &format!("{base}/passkeys/login/finish"),
                "post",
                Op::new(id("passkey_login_finish"), "Finish a passkey sign-in", n)
                    .json_body(obj(), true)
                    .returns(200, "Tokens and the record.", tok.clone()),
            );
        }
        let token_body = json!({ "type": "object", "required": ["token"], "properties": { "token": { "type": "string" } } });
        self.add(
            &format!("{base}/request-verification"),
            "post",
            Op::new(
                id("request_verification"),
                "Email a verification link (always 204)",
                n,
            )
            .json_body(email_body(), true)
            .no_content("Sent if the account exists and is unverified."),
        );
        self.add(
            &format!("{base}/confirm-verification"),
            "post",
            Op::new(id("confirm_verification"), "Confirm an email address", n)
                .json_body(token_body.clone(), true)
                .no_content("Verified."),
        );
        if o.methods.password {
            self.add(
                &format!("{base}/request-password-reset"),
                "post",
                Op::new(
                    id("request_password_reset"),
                    "Email a password reset link (always 204)",
                    n,
                )
                .json_body(email_body(), true)
                .no_content("Sent if the account exists."),
            );
            self.add(
                &format!("{base}/confirm-password-reset"),
                "post",
                Op::new(id("confirm_password_reset"), "Set a new password; ends every session", n)
                    .json_body(json!({ "type": "object", "required": ["token", "password"], "properties": { "token": { "type": "string" }, "password": { "type": "string", "format": "password" } } }), true)
                    .no_content("Changed."),
            );
        }
        self.add(
            &format!("{base}/request-email-change"),
            "post",
            Op::new(id("request_email_change"), "Email a link to confirm a new address (signed in)", n)
                .json_body(json!({ "type": "object", "required": ["new_email"], "properties": { "new_email": { "type": "string", "format": "email" } } }), true)
                .no_content("Sent if the address is free."),
        );
        self.add(
            &format!("{base}/confirm-email-change"),
            "post",
            Op::new(id("confirm_email_change"), "Confirm a new email address; ends every session", n)
                .json_body(json!({ "type": "object", "required": ["token"], "properties": { "token": { "type": "string" }, "password": { "type": "string", "format": "password", "description": "Required when the account has a password." } } }), true)
                .no_content("Changed."),
        );
        if o.methods.otp {
            self.add(
                &format!("{base}/request-otp"),
                "post",
                Op::new(id("request_otp"), "Email a one-time code", n)
                    .json_body(email_body(), true)
                    .returns(200, "An id to send with the code (also for unknown addresses).", json!({ "type": "object", "required": ["otp_id"], "properties": { "otp_id": { "type": "string" } } })),
            );
            self.add(
                &format!("{base}/auth-with-otp"),
                "post",
                Op::new(id("auth_with_otp"), "Sign in with a one-time code", n)
                    .json_body(json!({ "type": "object", "required": ["otp_id", "code"], "properties": { "otp_id": { "type": "string" }, "code": { "type": "string" } } }), true)
                    .returns(200, "Tokens and the record.", tok.clone()),
            );
        }
        if o.methods.magic_link {
            self.add(
                &format!("{base}/request-magic-link"),
                "post",
                Op::new(id("request_magic_link"), "Email a sign-in link to your app (always 204)", n)
                    .json_body(json!({ "type": "object", "required": ["email", "redirect_uri"], "properties": { "email": { "type": "string", "format": "email" }, "redirect_uri": { "type": "string", "format": "uri" } } }), true)
                    .no_content("Sent if the account exists."),
            );
            self.add(
                &format!("{base}/auth-with-magic-link"),
                "post",
                Op::new(
                    id("auth_with_magic_link"),
                    "Sign in with a magic link token",
                    n,
                )
                .json_body(token_body.clone(), true)
                .returns(200, "Tokens and the record.", tok.clone()),
            );
        }
        let cid = c.id.to_string();
        let mut providers: Vec<&str> = self
            .input
            .providers
            .iter()
            .filter(|p| p.collection_id == cid)
            .map(|p| p.name.as_str())
            .collect();
        providers.sort_unstable();
        if !providers.is_empty() {
            self.add(
                &format!("{base}/providers/{{provider}}/start"),
                "get",
                Op::new(id("provider_start"), "Start an external sign-in (browser navigation)", n)
                    .param(json!({ "name": "provider", "in": "path", "required": true, "schema": { "type": "string", "enum": providers } }))
                    .param(json!({ "name": "redirect_uri", "in": "query", "required": true, "schema": { "type": "string", "format": "uri" }, "description": "Allow-listed app URI; receives `#code=` or `#error=`." }))
                    .param(query("link", json!({ "type": "boolean" }), "Add the provider to the signed-in account."))
                    .param(query("format", json!({ "type": "string", "enum": ["json"] }), "Answer `{url}` instead of redirecting."))
                    .returns(200, "With `format=json`: where to send the browser.", json!({ "type": "object", "properties": { "url": { "type": "string" } } }))
                    .also(302, "Redirect to the provider."),
            );
            self.add(&format!("{base}/auth-with-exchange-code"), "post", Op::new(id("auth_with_exchange_code"), "Exchange a provider sign-in code for tokens", n).json_body(json!({ "type": "object", "required": ["code"], "properties": { "code": { "type": "string" } } }), true).returns(200, "Tokens and the record.", tok));
            self.add(&format!("{base}/identities"), "get", Op::new(id("identities_list"), "Your linked provider identities", n).returns(200, "Identities.", json!({ "type": "object", "properties": { "items": { "type": "array", "items": obj() } } })));
            self.add(
                &format!("{base}/identities/{{id}}"),
                "delete",
                Op::new(id("identity_delete"), "Unlink an identity", n)
                    .param(path_param("id", "Identity id."))
                    .no_content("Unlinked."),
            );
        }
    }

    fn statics(&mut self) {
        self.schemas.insert(
            "AuthMethods".into(),
            json!({
                "type": "object",
                "properties": {
                    "password": obj(), "passkey": obj(), "otp": obj(), "magic_link": obj(),
                    "providers": { "type": "array", "items": obj() },
                    "signup_enabled": { "type": "boolean" },
                },
            }),
        );
        self.add(
            "/api/health",
            "get",
            Op::new("health_get", "Health", "system").returns(200, "The server is up.", obj()),
        );
        self.add(
            "/api/info",
            "get",
            Op::new("info_get", "Instance information", "system").returns(
                200,
                "Name, version, public URL, auth collections.",
                obj(),
            ),
        );
        self.add(
            "/api/files/{collection}/{id}/{filename}",
            "get",
            Op::new("files_get", "Download a file", "files")
                .param(path_param("collection", "Collection name or id."))
                .param(uuid_param("id", "Record id."))
                .param(path_param("filename", "Stored filename."))
                .param(query(
                    "thumb",
                    json!({ "type": "string" }),
                    "A thumbnail size of the field, e.g. `100x100`.",
                ))
                .param(query(
                    "token",
                    json!({ "type": "string" }),
                    "File token for protected files (POST /api/files/token).",
                ))
                .param(query(
                    "download",
                    json!({ "type": "boolean" }),
                    "Send `Content-Disposition: attachment`.",
                ))
                .returns(
                    200,
                    "The file.",
                    json!({ "type": "string", "format": "binary" }),
                ),
        );
        if let Some(ok) = self
            .paths
            .get_mut("/api/files/{collection}/{id}/{filename}")
        {
            ok["get"]["responses"]["200"]["content"] = json!({ "application/octet-stream": { "schema": { "type": "string", "format": "binary" } } });
        }
        self.add(
            "/api/files/token",
            "post",
            Op::new("files_token_create", "A short-lived token for protected files", "files")
                .returns(200, "The token.", json!({ "type": "object", "required": ["token"], "properties": { "token": { "type": "string" }, "expires_in": { "type": "integer" } } })),
        );
        self.add(
            "/api/realtime",
            "get",
            Op::new(
                "realtime_connect",
                "Open the realtime event stream (SSE); see docs/realtime-protocol.md",
                "realtime",
            )
            .returns(
                200,
                "`text/event-stream`; the first event carries the `client_id`.",
                json!({ "type": "string" }),
            ),
        );
        if let Some(p) = self.paths.get_mut("/api/realtime") {
            p["get"]["responses"]["200"]["content"] =
                json!({ "text/event-stream": { "schema": { "type": "string" } } });
        }
        self.add(
            "/api/realtime",
            "post",
            Op::new("realtime_subscribe", "Set the subscriptions of an SSE connection", "realtime")
                .json_body(json!({
                    "type": "object",
                    "required": ["client_id", "subscriptions"],
                    "properties": {
                        "client_id": { "type": "string" },
                        "subscriptions": {
                            "type": "array",
                            "description": "Replaces the connection's subscriptions.",
                            "items": {
                                "type": "object",
                                "required": ["topic"],
                                "properties": {
                                    "topic": { "type": "string", "description": "`records:{collection}` or `records:{collection}/{record_id}`." },
                                    "filter": { "type": "string", "description": "Rule-language filter (no `@collection`)." },
                                    "expand": { "type": "string" },
                                    "fields": { "type": "string" },
                                },
                            },
                        },
                    },
                }), true)
                .no_content("Subscribed."),
        );
        self.add(
            "/api/batch",
            "post",
            Op::new("batch", "Run several record writes in one transaction", "records")
                .json_body(json!({
                    "type": "object",
                    "required": ["requests"],
                    "properties": { "requests": { "type": "array", "maxItems": 100, "items": {
                        "type": "object",
                        "required": ["method", "url"],
                        "properties": {
                            "method": { "type": "string", "enum": ["POST", "PATCH", "DELETE"] },
                            "url": { "type": "string", "description": "`/api/collections/{c}/records[/{id}]`" },
                            "body": obj(),
                        },
                    } } },
                }), true)
                .returns(200, "Results in order.", json!({ "type": "object", "properties": { "results": { "type": "array", "items": { "type": "object", "properties": { "status": { "type": "integer" }, "body": {} } } } } })),
        );
    }

    fn admin(&mut self) {
        let generic: &[(&str, &str, &str, &str, bool)] = &[
            (
                "/api/superusers/auth-with-password",
                "post",
                "superusers_auth_with_password",
                "Superuser sign-in",
                true,
            ),
            (
                "/api/superusers/auth-refresh",
                "post",
                "superusers_auth_refresh",
                "Rotate a superuser refresh token",
                true,
            ),
            (
                "/api/superusers/logout",
                "post",
                "superusers_logout",
                "End the superuser session",
                false,
            ),
            (
                "/api/superusers",
                "get",
                "superusers_list",
                "List superusers",
                false,
            ),
            (
                "/api/superusers",
                "post",
                "superusers_create",
                "Create a superuser",
                true,
            ),
            (
                "/api/superusers/{id}",
                "patch",
                "superusers_update",
                "Update a superuser",
                true,
            ),
            (
                "/api/superusers/{id}",
                "delete",
                "superusers_delete",
                "Delete a superuser",
                false,
            ),
            (
                "/api/collections",
                "get",
                "collections_list",
                "List collections",
                false,
            ),
            (
                "/api/collections",
                "post",
                "collections_create",
                "Create a collection",
                true,
            ),
            (
                "/api/collections/{collection}",
                "get",
                "collections_get",
                "Get a collection",
                false,
            ),
            (
                "/api/collections/{collection}",
                "patch",
                "collections_update",
                "Change a collection (`?dry_run=true` previews)",
                true,
            ),
            (
                "/api/collections/{collection}",
                "delete",
                "collections_delete",
                "Delete a collection (to the trash)",
                false,
            ),
            (
                "/api/collections/{collection}/history",
                "get",
                "collections_history",
                "Schema history",
                false,
            ),
            (
                "/api/collections/export",
                "get",
                "collections_export",
                "Export all collections",
                false,
            ),
            (
                "/api/collections/import",
                "post",
                "collections_import",
                "Import collections",
                true,
            ),
            (
                "/api/trash",
                "get",
                "trash_list",
                "Deleted collections",
                false,
            ),
            (
                "/api/trash/{id}/restore",
                "post",
                "trash_restore",
                "Restore a deleted collection",
                false,
            ),
            (
                "/api/trash/{id}",
                "delete",
                "trash_purge",
                "Purge a deleted collection",
                false,
            ),
            (
                "/api/rules/check",
                "post",
                "rules_check",
                "Compile a rule",
                true,
            ),
            (
                "/api/settings",
                "get",
                "settings_get",
                "All settings (secrets masked)",
                false,
            ),
            (
                "/api/settings",
                "patch",
                "settings_update",
                "Change settings",
                true,
            ),
            (
                "/api/settings/test-storage",
                "post",
                "settings_test_storage",
                "Test file storage settings",
                true,
            ),
            (
                "/api/settings/test-smtp",
                "post",
                "settings_test_smtp",
                "Send a test email",
                true,
            ),
            (
                "/api/files/usage",
                "get",
                "files_usage",
                "Stored files and bytes",
                false,
            ),
            ("/api/logs", "get", "logs_list", "Request logs", false),
            (
                "/api/metrics/summary",
                "get",
                "metrics_summary",
                "Dashboard numbers",
                false,
            ),
            (
                "/api/metrics/series",
                "get",
                "metrics_series",
                "Metric series",
                false,
            ),
            ("/api/audit", "get", "audit_list", "Audit trail", false),
            ("/api/backups", "get", "backups_list", "List backups", false),
            (
                "/api/backups",
                "post",
                "backups_create",
                "Create a backup",
                false,
            ),
            (
                "/api/backups/{name}",
                "get",
                "backups_download",
                "Download a backup",
                false,
            ),
            (
                "/api/backups/{name}",
                "delete",
                "backups_delete",
                "Delete a backup",
                false,
            ),
            (
                "/api/auth-providers",
                "get",
                "auth_providers_list",
                "List sign-in providers",
                false,
            ),
            (
                "/api/auth-providers",
                "post",
                "auth_providers_create",
                "Add a sign-in provider",
                true,
            ),
            (
                "/api/auth-providers/{id}",
                "patch",
                "auth_providers_update",
                "Change a sign-in provider",
                true,
            ),
            (
                "/api/auth-providers/{id}",
                "delete",
                "auth_providers_delete",
                "Remove a sign-in provider",
                false,
            ),
            (
                "/api/auth-providers/discover",
                "post",
                "auth_providers_discover",
                "Check an OpenID issuer",
                true,
            ),
        ];
        for (path, method, id, summary, body) in generic {
            let mut op = Op::new(*id, *summary, "admin").returns(
                200,
                "See the admin API documentation.",
                obj(),
            );
            if *method == "delete" {
                op = op.no_content("Done.");
            }
            for seg in path
                .split('/')
                .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
            {
                op = op.param(path_param(seg, ""));
            }
            if *body {
                op = op.json_body(obj(), true);
            }
            self.add(path, method, op);
        }
    }
}

/// Builds the document.
pub fn generate(input: &DocInput<'_>) -> Value {
    let mut b = Builder {
        input,
        paths: Map::new(),
        schemas: Map::new(),
    };
    problem_schemas(&mut b.schemas);
    b.statics();
    let mut collections: Vec<&Collection> = input.schema.all().iter().map(AsRef::as_ref).collect();
    collections.sort_by(|a, b| a.name.cmp(&b.name));
    // Records of every collection are referenced by expand schemas, so all
    // record schemas exist even when operations are left out.
    for c in &collections {
        b.collection(c);
    }
    if input.include_admin {
        b.admin();
    }
    let mut tags: Vec<Value> = vec![
        json!({ "name": "system", "description": "Health and instance information." }),
        json!({ "name": "records", "description": "Batch writes." }),
        json!({ "name": "files", "description": "File downloads and tokens." }),
        json!({ "name": "realtime", "description": "Server-sent events. WebSocket and collaboration: docs/realtime-protocol.md." }),
    ];
    for c in &collections {
        let kind = if is_auth(c) {
            "Auth collection"
        } else {
            "Collection"
        };
        tags.push(json!({ "name": c.name, "description": format!("{kind} `{}`.", c.name) }));
    }
    if input.include_admin {
        tags.push(json!({ "name": "admin", "description": "Superuser endpoints." }));
    }
    let mut doc = json!({
        "openapi": "3.1.0",
        "info": {
            "title": format!("{} API", input.app_name),
            "version": input.app_version,
            "description": "Generated by rackend from the current collections. Errors are RFC 9457 problem details (docs/errors.md). Filters use the rule language (docs/guide/rules.md).",
        },
        "tags": tags,
        "paths": b.paths,
        "components": {
            "schemas": b.schemas,
            "responses": {
                "Problem": {
                    "description": "An error, as RFC 9457 problem details.",
                    "content": { "application/problem+json": { "schema": r("Problem") } },
                },
            },
            "securitySchemes": {
                "bearerAuth": { "type": "http", "scheme": "bearer", "description": "An access token from a sign-in endpoint." },
                "cookieAuth": { "type": "apiKey", "in": "cookie", "name": "rk_session", "description": "Cookie mode (browsers); unsafe requests need a matching Origin." },
            },
        },
        "security": [{ "bearerAuth": [] }, { "cookieAuth": [] }, {}],
    });
    if let Some(u) = &input.public_url {
        doc["servers"] = json!([{ "url": u }]);
    }
    if input.version == Version::V30 {
        to_30(&mut doc);
        doc["openapi"] = json!("3.0.3");
    }
    doc
}

/// Rewrites 3.1 constructs as 3.0.3: type arrays with `null` become
/// `nullable`, `anyOf [X, null]` becomes `allOf [X]` + `nullable`.
fn to_30(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if let Some(Value::Array(types)) = m.get("type").cloned() {
                let rest: Vec<Value> = types
                    .iter()
                    .filter(|t| t.as_str() != Some("null"))
                    .cloned()
                    .collect();
                if rest.len() == 1 {
                    m.insert("type".into(), rest[0].clone());
                    if rest.len() < types.len() {
                        m.insert("nullable".into(), json!(true));
                    }
                }
            }
            if let Some(Value::Array(any)) = m.get("anyOf").cloned()
                && any.len() == 2
                && any.iter().any(|s| s == &json!({ "type": "null" }))
            {
                let inner = any
                    .into_iter()
                    .find(|s| s != &json!({ "type": "null" }))
                    .unwrap_or(Value::Null);
                m.remove("anyOf");
                m.insert("allOf".into(), json!([inner]));
                m.insert("nullable".into(), json!(true));
            }
            for (_, child) in m.iter_mut() {
                to_30(child);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(to_30),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case() {
        assert_eq!(pascal("posts"), "Posts");
        assert_eq!(pascal("user_profiles"), "UserProfiles");
    }

    #[test]
    fn nullable_30() {
        let mut v = json!({ "a": { "type": ["string", "null"] }, "b": { "anyOf": [{ "$ref": "#/x" }, { "type": "null" }] } });
        to_30(&mut v);
        assert_eq!(v["a"], json!({ "type": "string", "nullable": true }));
        assert_eq!(
            v["b"],
            json!({ "allOf": [{ "$ref": "#/x" }], "nullable": true })
        );
    }
}
