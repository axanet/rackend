//! Minimal server-rendered HTML for hosted pages (SPEC §10.2.5).

use crate::problem::ProblemBody;

/// Escapes text for HTML element content and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A human-readable error page showing title, detail and request id.
pub fn error_page(problem: &ProblemBody<'_>) -> String {
    let json = serde_json::to_value(problem).unwrap_or_default();
    let field = |k: &str| {
        json.get(k)
            .and_then(|v| v.as_str())
            .map(escape)
            .unwrap_or_default()
    };
    let status = json
        .get("status")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(500);
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{status} {title}</title></head><body>\
<main><h1>{title}</h1><p>{detail}</p><p><small>Request id: <code>{rid}</code></small></p></main>\
</body></html>",
        title = field("title"),
        detail = field("detail"),
        rid = field("request_id"),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_markup() {
        assert_eq!(
            super::escape("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
