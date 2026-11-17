//! Email (SPEC §11.4, §15): the mailer built from `smtp` settings, the
//! `email_templates`, and the SMTP test.

use std::sync::Arc;

use serde_json::{Value, json};

use super::{App, RequestCtx};
use crate::ports::{Email, Mailer, SmtpConfig, SmtpTls};
use crate::settings::MASK;
use crate::{Error, Result};

/// Escapes text for HTML.
pub(crate) fn escape_html(s: &str) -> String {
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

/// Replaces `{{name}}` with values; `escape` for HTML bodies.
fn fill(template: &str, vars: &[(&str, &str)], escape: bool) -> String {
    let mut out = template.to_owned();
    for (k, v) in vars {
        let value = if escape {
            escape_html(v)
        } else {
            (*v).to_owned()
        };
        out = out.replace(&format!("{{{{{k}}}}}"), &value);
    }
    out
}

fn smtp_from(
    section: &serde_json::Map<String, Value>,
    password: Option<String>,
) -> Option<SmtpConfig> {
    let s = |k: &str| {
        section
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let host = s("host");
    if host.trim().is_empty() {
        return None;
    }
    Some(SmtpConfig {
        host,
        port: section
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .unwrap_or(587),
        username: s("username"),
        password: password.unwrap_or_default(),
        tls: match section.get("tls").and_then(Value::as_str) {
            Some("tls") => SmtpTls::Tls,
            Some("none") => SmtpTls::None,
            _ => SmtpTls::StartTls,
        },
        from_name: s("from_name"),
        from_address: s("from_address"),
    })
}

impl App {
    /// The SMTP settings, `None` while no host is set.
    pub fn smtp_config(&self) -> Option<SmtpConfig> {
        let settings = self.settings();
        let section = settings.section("smtp")?;
        smtp_from(section, self.secret_setting("smtp", &["password"]))
    }

    /// Whether email can be sent.
    pub fn smtp_configured(&self) -> bool {
        self.mailer
            .read()
            .map_or_else(|p| p.into_inner().is_some(), |g| g.is_some())
    }

    /// Rebuilds the mailer from the settings.
    pub(crate) fn rebuild_mailer(&self) {
        let mailer = self.smtp_config().and_then(|cfg| match self.mailer_factory.build(&cfg) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::error!(error = %e, "cannot set up SMTP; email is unavailable until the settings are fixed");
                None
            }
        });
        match self.mailer.write() {
            Ok(mut g) => *g = mailer,
            Err(p) => *p.into_inner() = mailer,
        }
    }

    /// The mailer, or `smtp_not_configured`.
    pub(crate) fn mailer(&self) -> Result<Arc<dyn Mailer>> {
        self.mailer
            .read()
            .map_or_else(|p| p.into_inner().clone(), |g| g.clone())
            .ok_or(Error::NotConfigured("smtp"))
    }

    /// Renders template `name` (`verification`, `password_reset`, …).
    pub(crate) fn render_email(&self, name: &str, to: &str, vars: &[(&str, &str)]) -> Email {
        let settings = self.settings();
        let app_name = settings
            .string("app", "name")
            .unwrap_or_else(|| "rackend".into());
        let defaults = crate::settings::default_templates();
        let t = settings
            .section("email_templates")
            .and_then(|s| s.get(name))
            .cloned()
            .unwrap_or_else(|| defaults[name].clone());
        let field = |k: &str| {
            t.get(k)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .or_else(|| defaults[name][k].as_str())
                .unwrap_or_default()
                .to_owned()
        };
        let mut all: Vec<(&str, &str)> = vec![("app_name", app_name.as_str()), ("email", to)];
        all.extend_from_slice(vars);
        Email {
            to: to.to_owned(),
            subject: fill(&field("subject"), &all, false).replace(['\r', '\n'], " "),
            html: fill(&field("html"), &all, true),
            text: fill(&field("text"), &all, false),
        }
    }

    /// Sends in the background, so responses take the same time whether or
    /// not an email goes out (no account enumeration, SPEC §11.4).
    pub(crate) fn send_later(&self, email: Email) {
        let Ok(mailer) = self.mailer() else { return };
        self.spawner.spawn(
            "mail.send",
            Box::pin(async move {
                if let Err(e) = mailer.send(email).await {
                    tracing::warn!(error = %e, "sending an email failed");
                }
            }),
        );
    }

    /// `POST /api/settings/test-smtp {to, smtp?}`: sends a test email with the
    /// saved settings, or with `candidate` merged over them. Nothing is saved.
    pub async fn smtp_test(
        &self,
        ctx: &RequestCtx,
        to: &str,
        candidate: Option<Value>,
    ) -> Result<Value> {
        ctx.require_superuser()?;
        if !to.contains('@') {
            return Err(Error::field(
                "to",
                crate::error::FieldError::new("invalid_email", "Must be an email address."),
            ));
        }
        let settings = self.settings();
        let mut section = settings.section("smtp").cloned().unwrap_or_default();
        if let Some(c) = candidate.filter(|c| !c.is_null()) {
            let merged = settings
                .validate_patch(&json!({ "smtp": c }))
                .map_err(Error::Validation)?;
            if let Some((_, m)) = merged.into_iter().next() {
                section = m;
            }
        }
        let password = match section.get("password").and_then(Value::as_str) {
            Some(s) if crate::secrets::is_encrypted(s) => self.secrets.decrypt_str(s).ok(),
            Some(s) if s != MASK => Some(s.to_owned()),
            _ => self.secret_setting("smtp", &["password"]),
        };
        let Some(cfg) = smtp_from(&section, password) else {
            return Err(Error::NotConfigured("smtp"));
        };
        let mailer = match self.mailer_factory.build(&cfg) {
            Ok(m) => m,
            Err(e) => return Ok(json!({ "ok": false, "error": e.to_string() })),
        };
        let app_name = settings
            .string("app", "name")
            .unwrap_or_else(|| "rackend".into());
        let email = Email {
            to: to.to_owned(),
            subject: format!("Test email from {app_name}"),
            text: format!("This is a test email from {app_name}. SMTP works."),
            html: format!(
                "<p>This is a test email from {}. SMTP works.</p>",
                escape_html(&app_name)
            ),
        };
        let started = std::time::Instant::now();
        Ok(match mailer.send(email).await {
            Ok(()) => json!({ "ok": true, "ms": started.elapsed().as_millis() as u64 }),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_and_escapes() {
        let vars = [("link", "https://x/?a=1&b=<2>"), ("email", "a@b.c")];
        assert_eq!(
            fill("{{link}} {{email}} {{other}}", &vars, false),
            "https://x/?a=1&b=<2> a@b.c {{other}}"
        );
        assert_eq!(
            fill("<a href=\"{{link}}\">", &vars, true),
            "<a href=\"https://x/?a=1&amp;b=&lt;2&gt;\">"
        );
    }
}
