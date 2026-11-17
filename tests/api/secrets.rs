//! M2 acceptance: secrets at rest, signing keys, no secrets in logs.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use rackend_core::ports::{Clock, StoreExt, SystemClock};
use rackend_core::secrets::SecretBox;
use rackend_core::usecases::secrets::{SecretsInitError, init};
use serde_json::json;
use tracing_subscriber::fmt::MakeWriter;

use crate::common::*;

async fn audit_count(t: &TestApp, action: &str) -> u64 {
    let (_, _, a) = call(
        &t.router,
        Method::GET,
        &format!("/api/audit?action={action}"),
        Some(&t.token),
        None,
    )
    .await;
    a["total_items"].as_u64().unwrap()
}

#[tokio::test]
async fn signing_keys_and_secret_key_rotation() {
    let t = start().await;
    let store = Arc::clone(&t.app.store);
    let now = || SystemClock.now();

    // First boot: a local key, and the keys are encrypted with it.
    t.app.ensure_signing_keys().await.unwrap();
    t.app.ensure_signing_keys().await.unwrap();
    let keys = store.read(|tx| tx.keys_list()).await.unwrap();
    assert_eq!(keys.len(), 2, "one RS256 and one ES256, created once");
    assert!(
        keys.iter()
            .all(|k| rackend_core::secrets::is_encrypted(&k.private_enc))
    );
    assert!(
        keys.iter()
            .all(|k| t.app.secrets.decrypt(&k.private_enc).is_ok())
    );
    let jwk: serde_json::Value = serde_json::from_str(&keys[0].public_jwk).unwrap();
    assert_eq!(jwk["kid"], keys[0].kid.as_str());
    assert!(jwk.get("d").is_none(), "no private parts in the public JWK");
    assert_eq!(audit_count(&t, "keys.generate").await, 2);

    // Moving to an env key re-encrypts everything and drops the stored key.
    let env = SecretBox::generate_key();
    let boxed = init(store.as_ref(), Some(env), false, now()).await.unwrap();
    let keys = store.read(|tx| tx.keys_list()).await.unwrap();
    assert!(keys.iter().all(|k| boxed.decrypt(&k.private_enc).is_ok()));
    assert!(
        keys.iter()
            .all(|k| t.app.secrets.decrypt(&k.private_enc).is_err())
    );
    assert_eq!(audit_count(&t, "secrets.rekey").await, 1);
    // Restarting with the same key is a no-op.
    init(store.as_ref(), Some(env), false, now()).await.unwrap();
    assert_eq!(audit_count(&t, "secrets.rekey").await, 1);

    // A missing or wrong key refuses to start.
    let err = init(store.as_ref(), None, false, now()).await.unwrap_err();
    assert!(matches!(err, SecretsInitError::MissingKey), "{err}");
    let other = SecretBox::generate_key();
    let err = init(store.as_ref(), Some(other), false, now())
        .await
        .unwrap_err();
    assert!(matches!(err, SecretsInitError::WrongKey), "{err}");
    assert!(err.to_string().contains("RACKEND_RESET_SECRETS"));

    // Reset discards what cannot be decrypted; keys are generated again.
    init(store.as_ref(), Some(other), true, now())
        .await
        .unwrap();
    assert!(store.read(|tx| tx.keys_list()).await.unwrap().is_empty());
    assert_eq!(audit_count(&t, "secrets.reset").await, 1);
    init(store.as_ref(), Some(other), false, now())
        .await
        .unwrap();
}

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

/// Runs the auth flows with TRACE logging and checks that no password or
/// token reaches the output (SPEC §14.5).
#[tokio::test]
async fn no_secrets_in_logs() {
    let buf = Buf::default();
    let sub = rackend::logging::subscriber(
        tracing_subscriber::EnvFilter::new("trace"),
        rackend::config::LogFormat::Json,
        buf.clone(),
    );
    // Global, so the writer thread and blocking pool are captured too. Other
    // tests in the same process may log here as well; that is harmless.
    let _ = tracing::subscriber::set_global_default(sub);

    let t = start().await;
    let user_pw = "user password that must not leak";
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/records",
        None,
        Some(json!({"email": "ada@example.com", "password": user_pw})),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, login) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-with-password",
        None,
        Some(json!({"email": "ada@example.com", "password": user_pw})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let refresh = login["refresh_token"].as_str().unwrap();
    let (s, _, again) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-refresh",
        None,
        Some(json!({"refresh_token": refresh})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/users/auth-with-password",
        None,
        Some(json!({"email": "ada@example.com", "password": "a wrong password here"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    t.app.ensure_signing_keys().await.unwrap();

    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!out.is_empty(), "the subscriber captured nothing");
    let mut secrets = vec![
        ADMIN_PASSWORD.to_owned(),
        user_pw.to_owned(),
        "a wrong password here".to_owned(),
        t.token.clone(),
    ];
    for v in [&login, &again] {
        for k in ["access_token", "refresh_token"] {
            let tok = v[k].as_str().unwrap();
            secrets.push(tok.to_owned());
            // The secret half alone must not appear either. It is base64url
            // and may contain `_`, so split after `rk_xx_<session id>_`.
            let secret = tok.splitn(4, '_').nth(3).unwrap();
            assert_eq!(secret.len(), 43, "{tok}");
            secrets.push(secret.to_owned());
        }
    }
    for s in &secrets {
        assert!(
            !out.contains(s.as_str()),
            "a secret leaked into the logs: {s}"
        );
    }
    assert!(!out.contains("PRIVATE KEY"));
}
