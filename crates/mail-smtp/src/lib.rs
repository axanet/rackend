//! `rackend-mail-smtp`: the core `Mailer` port over SMTP with `lettre`
//! (SPEC §11.4). STARTTLS, implicit TLS or plain connections; rustls with the
//! Mozilla root store.

use std::sync::Arc;
use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport as _, Message, Tokio1Executor};
use rackend_core::ports::{
    BoxFuture, Email, MailError, Mailer, MailerFactory, SmtpConfig, SmtpTls,
};

/// Connection timeout.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Builds [`SmtpMailer`]s.
#[derive(Debug, Clone, Copy, Default)]
pub struct SmtpFactory;

impl MailerFactory for SmtpFactory {
    fn build(&self, config: &SmtpConfig) -> Result<Arc<dyn Mailer>, MailError> {
        Ok(Arc::new(SmtpMailer::new(config)?))
    }
}

/// Sends email through one SMTP server (connections are pooled).
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl std::fmt::Debug for SmtpMailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpMailer")
            .field("from", &self.from.email.to_string())
            .finish_non_exhaustive()
    }
}

fn err(e: impl std::fmt::Display) -> MailError {
    MailError(e.to_string())
}

impl SmtpMailer {
    /// A mailer for `config`; nothing is sent or connected yet.
    pub fn new(config: &SmtpConfig) -> Result<Self, MailError> {
        let builder = match config.tls {
            SmtpTls::Tls => {
                AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host).map_err(err)?
            }
            SmtpTls::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host).map_err(err)?
            }
            SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host),
        };
        let mut builder = builder.port(config.port).timeout(Some(TIMEOUT));
        if !config.username.is_empty() {
            builder = builder.credentials(Credentials::new(
                config.username.clone(),
                config.password.clone(),
            ));
        }
        let address = config
            .from_address
            .parse()
            .map_err(|_| MailError("smtp.from_address is not a valid email address".into()))?;
        let name =
            (!config.from_name.trim().is_empty()).then(|| config.from_name.trim().to_owned());
        Ok(Self {
            transport: builder.build(),
            from: Mailbox::new(name, address),
        })
    }
}

/// The message: a `multipart/alternative` with text and HTML.
pub fn message(from: &Mailbox, email: &Email) -> Result<Message, MailError> {
    let to: Mailbox = email
        .to
        .parse()
        .map_err(|_| MailError("the recipient is not a valid email address".into()))?;
    Message::builder()
        .from(from.clone())
        .to(to)
        .subject(email.subject.clone())
        .multipart(
            MultiPart::alternative()
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_PLAIN)
                        .body(email.text.clone()),
                )
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(email.html.clone()),
                ),
        )
        .map_err(err)
}

impl Mailer for SmtpMailer {
    fn send(&self, email: Email) -> BoxFuture<'_, Result<(), MailError>> {
        Box::pin(async move {
            let msg = message(&self.from, &email)?;
            self.transport.send(msg).await.map_err(err)?;
            tracing::debug!("email sent");
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    use super::*;

    fn config(port: u16) -> SmtpConfig {
        SmtpConfig {
            host: "127.0.0.1".into(),
            port,
            username: String::new(),
            password: String::new(),
            tls: SmtpTls::None,
            from_name: "rackend".into(),
            from_address: "noreply@example.com".into(),
        }
    }

    #[test]
    fn rejects_bad_from() {
        let mut c = config(25);
        c.from_address = "nope".into();
        assert!(SmtpMailer::new(&c).is_err());
    }

    #[test]
    fn builds_multipart() {
        let from: Mailbox = "rackend <noreply@example.com>"
            .parse()
            .unwrap_or_else(|_| unreachable!());
        let m = message(
            &from,
            &Email {
                to: "a@example.com".into(),
                subject: "Hi".into(),
                html: "<p>x</p>".into(),
                text: "x".into(),
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let raw = String::from_utf8_lossy(&m.formatted()).to_string();
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("Subject: Hi"));
    }

    /// A tiny SMTP server that accepts one message and returns its DATA.
    async fn fake_server() -> (u16, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let port = listener.local_addr().map(|a| a.port()).unwrap_or_default();
        let task = tokio::spawn(async move {
            let Ok((sock, _)) = listener.accept().await else {
                return String::new();
            };
            let (r, mut w) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            let _ = w.write_all(b"220 test ESMTP\r\n").await;
            let mut data = String::new();
            let mut in_data = false;
            while let Ok(Some(line)) = lines.next_line().await {
                if in_data {
                    if line == "." {
                        in_data = false;
                        let _ = w.write_all(b"250 OK\r\n").await;
                    } else {
                        data.push_str(&line);
                        data.push('\n');
                    }
                    continue;
                }
                let upper = line.to_ascii_uppercase();
                let reply: &[u8] = if upper.starts_with("EHLO") {
                    b"250-test\r\n250 8BITMIME\r\n"
                } else if upper.starts_with("DATA") {
                    in_data = true;
                    b"354 go\r\n"
                } else if upper.starts_with("QUIT") {
                    let _ = w.write_all(b"221 bye\r\n").await;
                    break;
                } else {
                    b"250 OK\r\n"
                };
                let _ = w.write_all(reply).await;
            }
            data
        });
        (port, task)
    }

    #[tokio::test]
    async fn sends_over_plain_smtp() {
        let (port, server) = fake_server().await;
        let mailer = SmtpMailer::new(&config(port)).unwrap_or_else(|e| panic!("{e}"));
        mailer
            .send(Email {
                to: "user@example.com".into(),
                subject: "Your code".into(),
                html: "<p>123456</p>".into(),
                text: "123456".into(),
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        drop(mailer);
        let data = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_default();
        assert!(data.contains("Subject: Your code"), "{data}");
        assert!(data.contains("To: user@example.com"), "{data}");
    }
}
