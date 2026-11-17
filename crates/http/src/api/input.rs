//! Record bodies: JSON, or `multipart/form-data` with files (SPEC §10.1,
//! §12.3). Files are streamed to a per-request temporary directory while
//! their SHA-256 is computed and their size limit enforced.

use std::path::{Path, PathBuf};

use axum::extract::{FromRequest, Multipart, Request};
use axum::http::header::CONTENT_TYPE;
use rackend_core::usecases::files::Upload;
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

use crate::AppState;
use crate::problem::{ApiError, ProblemCode};

/// Bytes kept for type sniffing.
const HEAD_BYTES: usize = 512;
/// Name of the JSON part of a multipart body.
pub const JSON_PART: &str = "@json";

/// Deletes the request's upload directory when dropped.
#[derive(Debug)]
pub struct TmpDir(PathBuf);

impl TmpDir {
    fn create(base: &Path) -> std::io::Result<Self> {
        let dir = base.join("uploads").join(uuid::Uuid::now_v7().to_string());
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error = %e, dir = %self.0.display(), "cannot remove an upload directory");
        }
    }
}

/// A parsed record body.
#[derive(Debug)]
pub struct RecordInput {
    /// JSON fields.
    pub body: Map<String, Value>,
    /// Uploaded files.
    pub uploads: Vec<Upload>,
    /// Keeps the temporary files alive until the use case is done.
    pub tmp: Option<TmpDir>,
}

fn too_large(max: u64) -> ApiError {
    rackend_core::Error::PayloadTooLarge { max_bytes: max }.into()
}

fn bad(detail: impl Into<String>) -> ApiError {
    ApiError::with_detail(ProblemCode::BadRequest, detail)
}

fn object(v: Value) -> Result<Map<String, Value>, ApiError> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(bad("The body must be a JSON object.")),
    }
}

/// Reads a JSON or multipart record body for collection `key`.
pub async fn record_input(
    state: &AppState,
    key: &str,
    req: Request,
) -> Result<RecordInput, ApiError> {
    let ct = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let json_limit = state.max_json_body_bytes();
    if ct.starts_with("multipart/form-data") {
        return multipart(state, key, req, json_limit).await;
    }
    if !(ct.starts_with("application/json") || ct.contains("+json")) {
        return Err(ApiError::with_detail(
            ProblemCode::UnsupportedMediaType,
            "Send application/json, or multipart/form-data with files.",
        ));
    }
    let bytes = axum::body::to_bytes(
        req.into_body(),
        usize::try_from(json_limit).unwrap_or(usize::MAX),
    )
    .await
    .map_err(|_| too_large(json_limit))?;
    let v: Value = serde_json::from_slice(&bytes).map_err(|e| bad(format!("Invalid JSON: {e}")))?;
    Ok(RecordInput {
        body: object(v)?,
        uploads: Vec::new(),
        tmp: None,
    })
}

async fn multipart(
    state: &AppState,
    key: &str,
    req: Request,
    json_limit: u64,
) -> Result<RecordInput, ApiError> {
    let rackend_core::usecases::files::UploadLimits {
        fields: limits,
        total: total_limit,
    } = state.app().upload_limits(key)?;
    let mut mp = Multipart::from_request(req, state)
        .await
        .map_err(|e| bad(format!("Invalid multipart body: {e}")))?;
    let tmp = TmpDir::create(&state.tmp_dir()).map_err(|e| ApiError::internal(&e))?;
    let mut body = Map::new();
    let mut uploads = Vec::new();
    let mut total: u64 = 0;
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return Err(bad(format!("Invalid multipart body: {e}"))),
        };
        let name = field.name().unwrap_or_default().to_owned();
        // `@json` is JSON even when the client sent it as a file part
        // (browsers name Blob parts "blob").
        let filename = field
            .file_name()
            .filter(|_| name != JSON_PART)
            .map(str::to_owned);
        match filename {
            None => {
                // Text parts: the `@json` object, or plain field values.
                let text = read_text(field, json_limit, &mut total, total_limit).await?;
                if name == JSON_PART {
                    for (k, v) in object(
                        serde_json::from_str(&text)
                            .map_err(|e| bad(format!("Invalid JSON in @json: {e}")))?,
                    )? {
                        body.insert(k, v);
                    }
                } else if !name.is_empty() {
                    let compiled = state.app().schema.get();
                    let c = compiled.schema.by_id_or_name(key);
                    let value = coerce(c.map(AsRef::as_ref), &name, text);
                    let multi = c
                        .and_then(|c| c.field(field_name(&name)))
                        .is_some_and(|f| f.is_multi());
                    match body.get_mut(&name) {
                        // Repeated parts of a multi-value field form an array.
                        Some(Value::Array(a)) if multi => match value {
                            Value::Array(more) => a.extend(more),
                            v => a.push(v),
                        },
                        Some(prev) if multi => {
                            let first = prev.take();
                            *prev = Value::Array(vec![first, value]);
                        }
                        _ => {
                            body.insert(name, value);
                        }
                    }
                }
            }
            Some(original) => {
                let Some(&(max, _)) = limits.get(&name) else {
                    return Err(ApiError::from(rackend_core::Error::field(
                        name,
                        rackend_core::error::FieldError::new(
                            "not_a_file_field",
                            "This field does not take files.",
                        ),
                    )));
                };
                let path = tmp.0.join(format!("part-{}", uploads.len()));
                uploads.push(
                    stream_file(field, &path, &name, &original, max, &mut total, total_limit)
                        .await?,
                );
            }
        }
    }
    Ok(RecordInput {
        body,
        uploads,
        tmp: Some(tmp),
    })
}

/// The field a body key refers to (`tags+`, `+tags`, `views-` → the name).
fn field_name(key: &str) -> &str {
    key.trim_start_matches('+').trim_end_matches(['+', '-'])
}

/// Types a text part by its field, so form clients can send numbers,
/// booleans and JSON as text (`"42"`, `"true"`, `"[1,2]"`).
fn coerce(c: Option<&rackend_core::domain::Collection>, name: &str, text: String) -> Value {
    use rackend_core::domain::FieldKind;
    let Some(f) = c.and_then(|c| c.field(field_name(name))) else {
        return Value::String(text);
    };
    let t = text.trim();
    match &f.kind {
        FieldKind::Number { .. } if t.is_empty() => Value::Null,
        FieldKind::Number { .. } => t
            .parse::<i64>()
            .map(Value::from)
            .ok()
            .or_else(|| {
                t.parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
            })
            .unwrap_or(Value::String(text)),
        FieldKind::Bool => match t.to_ascii_lowercase().as_str() {
            "true" | "1" | "on" => Value::Bool(true),
            "false" | "0" | "" => Value::Bool(false),
            _ => Value::String(text),
        },
        FieldKind::Json { .. } => serde_json::from_str(t).unwrap_or(Value::String(text)),
        _ if f.is_multi() && t.starts_with('[') => {
            serde_json::from_str(t).unwrap_or(Value::String(text))
        }
        // One part of a multi-value field is a one-element list.
        FieldKind::Select { .. } | FieldKind::Relation { .. } if f.is_multi() => {
            Value::Array(vec![Value::String(text)])
        }
        _ => Value::String(text),
    }
}

async fn read_text(
    mut field: axum::extract::multipart::Field<'_>,
    limit: u64,
    total: &mut u64,
    total_limit: u64,
) -> Result<String, ApiError> {
    let mut buf = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| bad(format!("Invalid multipart body: {e}")))?
    {
        buf.extend_from_slice(&chunk);
        *total += chunk.len() as u64;
        if buf.len() as u64 > limit {
            return Err(too_large(limit));
        }
        if *total > total_limit {
            return Err(too_large(total_limit));
        }
    }
    String::from_utf8(buf).map_err(|_| bad("Text parts must be UTF-8."))
}

async fn stream_file(
    mut field: axum::extract::multipart::Field<'_>,
    path: &Path,
    name: &str,
    original: &str,
    max: u64,
    total: &mut u64,
    total_limit: u64,
) -> Result<Upload, ApiError> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| ApiError::internal(&e))?;
    let mut hasher = Sha256::new();
    let mut head = Vec::with_capacity(HEAD_BYTES);
    let mut size: u64 = 0;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| bad(format!("Invalid multipart body: {e}")))?
    {
        size += chunk.len() as u64;
        *total += chunk.len() as u64;
        // Abort early: nothing beyond the limit is written (SPEC §12.3).
        if size > max {
            return Err(too_large(max));
        }
        if *total > total_limit {
            return Err(too_large(total_limit));
        }
        if head.len() < HEAD_BYTES {
            let take = (HEAD_BYTES - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| ApiError::internal(&e))?;
    }
    file.flush().await.map_err(|e| ApiError::internal(&e))?;
    Ok(Upload {
        field: name.to_owned(),
        original_name: if original.is_empty() {
            "file".into()
        } else {
            original.to_owned()
        },
        path: path.to_path_buf(),
        size,
        sha256: hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        head,
    })
}
