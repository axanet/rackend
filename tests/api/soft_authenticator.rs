//! A minimal software WebAuthn authenticator for tests: ES256 key, "none"
//! attestation, user verification always performed. Hand-written CBOR keeps
//! the dev dependencies small.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

/// Flags: user present, user verified.
const UP_UV: u8 = 0x01 | 0x04;
/// Flag: attested credential data included.
const AT: u8 = 0x40;

pub struct SoftAuthenticator {
    key: SigningKey,
    pub credential_id: Vec<u8>,
    /// The user handle the credential was registered for.
    pub user_handle: Option<Vec<u8>>,
    origin: String,
    rp_id: String,
    counter: u32,
}

fn b64(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

fn unb64(s: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .expect("base64url")
}

fn head(major: u8, n: u64) -> Vec<u8> {
    let m = major << 5;
    match n {
        0..=23 => vec![m | n as u8],
        24..=0xff => vec![m | 24, n as u8],
        0x100..=0xffff => {
            let mut v = vec![m | 25];
            v.extend_from_slice(&(n as u16).to_be_bytes());
            v
        }
        _ => {
            let mut v = vec![m | 26];
            v.extend_from_slice(&(n as u32).to_be_bytes());
            v
        }
    }
}

fn int(i: i64) -> Vec<u8> {
    if i >= 0 {
        head(0, i as u64)
    } else {
        head(1, (-1 - i) as u64)
    }
}

fn bytes(b: &[u8]) -> Vec<u8> {
    let mut v = head(2, b.len() as u64);
    v.extend_from_slice(b);
    v
}

fn text(s: &str) -> Vec<u8> {
    let mut v = head(3, s.len() as u64);
    v.extend_from_slice(s.as_bytes());
    v
}

fn map(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut v = head(5, entries.len() as u64);
    for (k, val) in entries {
        v.extend_from_slice(k);
        v.extend_from_slice(val);
    }
    v
}

impl SoftAuthenticator {
    /// An authenticator for the relying party at `origin` (e.g. `https://rk.example.com`).
    pub fn new(origin: &str, seed: u8) -> Self {
        let rp_id = origin
            .split("://")
            .nth(1)
            .and_then(|h| h.split([':', '/']).next())
            .expect("origin host")
            .to_owned();
        Self {
            key: SigningKey::from_slice(&[seed; 32]).expect("valid scalar"),
            credential_id: (0..32).map(|i| i ^ seed).collect(),
            user_handle: None,
            origin: origin.to_owned(),
            rp_id,
            counter: 0,
        }
    }

    fn client_data(&self, kind: &str, challenge: &str) -> Vec<u8> {
        json!({
            "type": kind,
            "challenge": challenge,
            "origin": self.origin,
            "crossOrigin": false,
        })
        .to_string()
        .into_bytes()
    }

    fn auth_data(&mut self, flags: u8, attested: Option<Vec<u8>>) -> Vec<u8> {
        self.counter += 1;
        let mut d = Sha256::digest(self.rp_id.as_bytes()).to_vec();
        d.push(flags);
        d.extend_from_slice(&self.counter.to_be_bytes());
        if let Some(a) = attested {
            d.extend_from_slice(&a);
        }
        d
    }

    /// Answers `navigator.credentials.create` for the `options` returned by
    /// `.../passkeys/register/start`.
    pub fn register(&mut self, options: &Value) -> Value {
        let pk = &options["publicKey"];
        let challenge = pk["challenge"].as_str().expect("challenge");
        let user_id = pk["user"]["id"].as_str().expect("user id");
        self.user_handle = Some(unb64(user_id));

        let point = self.key.verifying_key().to_sec1_point(false);
        let (x, y) = (point.x().expect("x"), point.y().expect("y"));
        let cose = map(&[
            (int(1), int(2)),
            (int(3), int(-7)),
            (int(-1), int(1)),
            (int(-2), bytes(x)),
            (int(-3), bytes(y)),
        ]);
        let mut attested = vec![0u8; 16]; // AAGUID
        attested.extend_from_slice(&(self.credential_id.len() as u16).to_be_bytes());
        attested.extend_from_slice(&self.credential_id);
        attested.extend_from_slice(&cose);
        let auth_data = self.auth_data(UP_UV | AT, Some(attested));
        let att_obj = map(&[
            (text("fmt"), text("none")),
            (text("attStmt"), map(&[])),
            (text("authData"), bytes(&auth_data)),
        ]);
        let client_data = self.client_data("webauthn.create", challenge);
        json!({
            "id": b64(&self.credential_id),
            "rawId": b64(&self.credential_id),
            "type": "public-key",
            "extensions": {},
            "response": {
                "attestationObject": b64(&att_obj),
                "clientDataJSON": b64(&client_data),
            },
        })
    }

    /// Answers `navigator.credentials.get` for the `options` returned by
    /// `.../passkeys/login/start`.
    pub fn login(&mut self, options: &Value) -> Value {
        let challenge = options["publicKey"]["challenge"]
            .as_str()
            .expect("challenge");
        let auth_data = self.auth_data(UP_UV, None);
        let client_data = self.client_data("webauthn.get", challenge);
        let mut signed = auth_data.clone();
        signed.extend_from_slice(&Sha256::digest(&client_data));
        let sig: Signature = self.key.sign(&signed);
        json!({
            "id": b64(&self.credential_id),
            "rawId": b64(&self.credential_id),
            "type": "public-key",
            "extensions": {},
            "response": {
                "authenticatorData": b64(&auth_data),
                "clientDataJSON": b64(&client_data),
                "signature": b64(sig.to_der().as_bytes()),
                "userHandle": self.user_handle.as_deref().map(b64),
            },
        })
    }
}
