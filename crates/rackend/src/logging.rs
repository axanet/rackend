//! Structured logging (SPEC §3.4, §14.5).
//!
//! - JSON (one object per line) or human-readable output.
//! - Fields of the enclosing spans are merged into every line, so every log
//!   line inside a request carries `request_id`.
//! - Fields whose name contains `password`, `token`, `secret`,
//!   `authorization` or `cookie` are replaced with `[redacted]`.

use std::fmt;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{FmtContext, FormattedFields, MakeWriter};
use tracing_subscriber::registry::LookupSpan;

use crate::config::LogFormat;

/// Field names containing one of these are redacted.
const REDACTED: &[&str] = &["password", "token", "secret", "authorization", "cookie"];

/// Whether a field with this name must be redacted.
pub fn is_redacted(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    REDACTED.iter().any(|r| lower.contains(r))
}

/// Installs the global subscriber.
pub fn init(level: &str, format: LogFormat) -> anyhow::Result<()> {
    install(level, format, std::io::stdout).map_err(|e| anyhow::anyhow!("logging: {e}"))
}

fn install<W>(
    level: &str,
    format: LogFormat,
    writer: W,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_new(level)?;
    let sub = subscriber(filter, format, writer);
    tracing::subscriber::set_global_default(sub)?;
    Ok(())
}

fn subscriber<W>(
    filter: EnvFilter,
    format: LogFormat,
    writer: W,
) -> Box<dyn Subscriber + Send + Sync>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    match format {
        LogFormat::Json => Box::new(
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .fmt_fields(JsonFields)
                .event_format(JsonEvents)
                .finish(),
        ),
        LogFormat::Pretty => Box::new(
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_ansi(true)
                .fmt_fields(PrettyFields)
                .finish(),
        ),
    }
}

/// Collects fields into a JSON map with redaction.
struct JsonVisitor<'a>(&'a mut Map<String, Value>);

impl JsonVisitor<'_> {
    fn put(&mut self, field: &Field, value: Value) {
        let v = if is_redacted(field.name()) {
            Value::String("[redacted]".into())
        } else {
            value
        };
        self.0.insert(field.name().to_owned(), v);
    }
}

impl Visit for JsonVisitor<'_> {
    fn record_f64(&mut self, f: &Field, v: f64) {
        self.put(
            f,
            serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number),
        );
    }
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.put(f, Value::from(v));
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.put(f, Value::from(v));
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.put(f, Value::from(v));
    }
    fn record_str(&mut self, f: &Field, v: &str) {
        self.put(f, Value::from(v));
    }
    fn record_error(&mut self, f: &Field, v: &(dyn std::error::Error + 'static)) {
        self.put(f, Value::from(v.to_string()));
    }
    fn record_debug(&mut self, f: &Field, v: &dyn fmt::Debug) {
        self.put(f, Value::from(format!("{v:?}")));
    }
}

/// Formats span fields as a JSON object (stored per span by the registry).
struct JsonFields;

impl<'w> FormatFields<'w> for JsonFields {
    fn format_fields<R: RecordFields>(&self, mut writer: Writer<'w>, fields: R) -> fmt::Result {
        let mut map = Map::new();
        fields.record(&mut JsonVisitor(&mut map));
        write!(writer, "{}", Value::Object(map))
    }

    fn add_fields(
        &self,
        current: &'w mut FormattedFields<Self>,
        fields: &tracing::span::Record<'_>,
    ) -> fmt::Result {
        let mut map = match serde_json::from_str::<Value>(&current.fields) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        fields.record(&mut JsonVisitor(&mut map));
        current.fields = Value::Object(map).to_string();
        Ok(())
    }
}

/// One JSON object per event: `ts`, `level`, `target`, span fields, event fields.
struct JsonEvents;

impl<S> FormatEvent<S, JsonFields> for JsonEvents
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, JsonFields>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut out = Map::new();
        out.insert(
            "ts".into(),
            Value::from(rackend_core::time::format_timestamp(
                time::OffsetDateTime::now_utc(),
            )),
        );
        let meta = event.metadata();
        out.insert("level".into(), Value::from(meta.level().as_str()));
        out.insert("target".into(), Value::from(meta.target()));

        if let Some(scope) = ctx.event_scope() {
            let mut names = Vec::new();
            for span in scope.from_root() {
                names.push(Value::from(span.name()));
                let ext = span.extensions();
                if let Some(fields) = ext.get::<FormattedFields<JsonFields>>()
                    && let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&fields.fields)
                {
                    out.extend(m);
                }
            }
            if !names.is_empty() {
                out.insert("spans".into(), Value::Array(names));
            }
        }

        let mut fields = Map::new();
        event.record(&mut JsonVisitor(&mut fields));
        out.extend(fields);
        writeln!(writer, "{}", Value::Object(out))
    }
}

/// Human-readable `key=value` fields with redaction.
struct PrettyFields;

impl<'w> FormatFields<'w> for PrettyFields {
    fn format_fields<R: RecordFields>(&self, mut writer: Writer<'w>, fields: R) -> fmt::Result {
        let mut v = PrettyVisitor {
            writer: &mut writer,
            first: true,
            result: Ok(()),
        };
        fields.record(&mut v);
        v.result
    }
}

struct PrettyVisitor<'a, 'w> {
    writer: &'a mut Writer<'w>,
    first: bool,
    result: fmt::Result,
}

impl Visit for PrettyVisitor<'_, '_> {
    fn record_str(&mut self, f: &Field, v: &str) {
        if f.name() == "message" {
            self.record_debug(f, &format_args!("{v}"));
        } else {
            self.record_debug(f, &v);
        }
    }

    fn record_debug(&mut self, f: &Field, v: &dyn fmt::Debug) {
        if self.result.is_err() {
            return;
        }
        let sep = if self.first { "" } else { " " };
        self.first = false;
        self.result = if f.name() == "message" {
            write!(self.writer, "{sep}{v:?}")
        } else if is_redacted(f.name()) {
            write!(self.writer, "{sep}{}=[redacted]", f.name())
        } else {
            write!(self.writer, "{sep}{}={v:?}", f.name())
        };
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture(format: LogFormat, f: impl FnOnce()) -> String {
        let buf = Buf::default();
        let sub = subscriber(EnvFilter::new("trace"), format, buf.clone());
        tracing::subscriber::with_default(sub, f);
        String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
    }

    const SECRET: &str = "hunter2-very-secret";

    fn log_secrets() {
        let span =
            tracing::info_span!("http.request", request_id = "rid-1", authorization = SECRET);
        let _g = span.enter();
        tracing::info!(
            password = SECRET,
            access_token = SECRET,
            user = "alice",
            "login attempt"
        );
        tracing::info!(cookie = %SECRET, client_secret = ?SECRET, "more");
    }

    #[test]
    fn json_redacts_and_carries_span_fields() {
        let out = capture(LogFormat::Json, log_secrets);
        assert!(!out.contains(SECRET), "{out}");
        let first: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!(first["request_id"], "rid-1");
        assert_eq!(first["password"], "[redacted]");
        assert_eq!(first["access_token"], "[redacted]");
        assert_eq!(first["authorization"], "[redacted]");
        assert_eq!(first["user"], "alice");
        assert_eq!(first["message"], "login attempt");
        assert_eq!(first["level"], "INFO");
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn pretty_redacts() {
        let out = capture(LogFormat::Pretty, log_secrets);
        assert!(!out.contains(SECRET), "{out}");
        assert!(out.contains("password=[redacted]"), "{out}");
        assert!(out.contains("login attempt"), "{out}");
    }
}
