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

/// Styles shared by server-rendered pages. Tokens follow the vault note
/// "17 Brand/Visual Identity" (values copied from the homepage). Fonts fall
/// back to system stacks: the self-hosted Geist files live in the UI bundle.
pub const PAGE_CSS: &str = ":root{color-scheme:light dark;\
--bg:light-dark(#fafaf9,#0a0b0d);--bg-elev:light-dark(#ffffff,#101114);\
--surface:light-dark(#f3f3f1,#14161a);--border:light-dark(#e3e3df,#22252b);\
--border-strong:light-dark(#cfcfca,#30343c);--fg:light-dark(#0b0c0e,#ededef);\
--fg-muted:light-dark(#52555c,#a0a3ab);--fg-subtle:light-dark(#6e7178,#6f727b);\
--accent:light-dark(#0a8f5f,#3ddc97);--danger:light-dark(#c4314b,#ff6b81);\
--font-sans:'Geist Variable','Geist',ui-sans-serif,system-ui,-apple-system,'Segoe UI',sans-serif;\
--font-mono:'Geist Mono Variable','Geist Mono',ui-monospace,'SF Mono',Menlo,Consolas,monospace}\
*{box-sizing:border-box}\
body{margin:0;min-height:100vh;display:grid;place-items:center;padding:clamp(16px,4vw,32px);\
background:var(--bg);color:var(--fg);font:16px/1.6 var(--font-sans);-webkit-font-smoothing:antialiased}\
main{width:min(480px,100%);display:grid;gap:12px;padding:32px;background:var(--bg-elev);\
border:1px solid var(--border);border-radius:10px}\
.mark{display:flex;align-items:center;gap:9px;margin-bottom:8px;font:600 17px/1 var(--font-mono);letter-spacing:-0.03em}\
.eyebrow{font:500 11px/1.2 var(--font-mono);letter-spacing:.08em;text-transform:uppercase;color:var(--fg-subtle)}\
.eyebrow b{font-weight:500;color:var(--danger)}\
h1{margin:0;font-size:1.6rem;font-weight:600;letter-spacing:-.03em;line-height:1.15}\
p{margin:0;color:var(--fg-muted)}\
.rid{display:inline-block;padding:2px 9px;border:1px solid var(--border);border-radius:999px;\
background:var(--surface);font:12px/1.5 var(--font-mono);color:var(--fg-muted)}\
a{color:inherit;text-underline-offset:3px}";

/// The placeholder rack mark (Visual Identity: Logo), inline SVG.
pub const MARK_SVG: &str = "<svg width=\"22\" height=\"22\" viewBox=\"0 0 32 32\" aria-hidden=\"true\">\
<rect x=\"4\" y=\"4.5\" width=\"24\" height=\"6.5\" rx=\"2\" fill=\"currentColor\"/>\
<rect x=\"4\" y=\"12.75\" width=\"24\" height=\"6.5\" rx=\"2\" fill=\"currentColor\" opacity=\".72\"/>\
<rect x=\"4\" y=\"21\" width=\"24\" height=\"6.5\" rx=\"2\" fill=\"currentColor\" opacity=\".44\"/>\
<circle cx=\"23.5\" cy=\"7.75\" r=\"1.7\" fill=\"var(--accent)\"/></svg>";

/// A human-readable error page showing title, detail and request id.
///
/// The styles are inline: the admin UI CSP allows inline styles. The hosted
/// auth pages (M8) have `style-src 'self'` and will serve this CSS as a file.
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
<title>{status} {title} · rackend</title><style>{css}</style></head><body>\
<main><div class=\"mark\">{mark}rackend</div>\
<span class=\"eyebrow\"><b>{status}</b> / {code}</span>\
<h1>{title}</h1><p>{detail}</p>\
<p><span class=\"rid\">request {rid}</span></p>\
</main></body></html>",
        css = PAGE_CSS,
        mark = MARK_SVG,
        title = field("title"),
        detail = field("detail"),
        code = field("code"),
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
