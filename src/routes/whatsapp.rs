use axum::{
    extract::Query,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

#[derive(Deserialize)]
pub struct VerifyParams {
    #[serde(rename = "hub.mode")]
    mode: Option<String>,
    #[serde(rename = "hub.challenge")]
    challenge: Option<String>,
    #[serde(rename = "hub.verify_token")]
    token: Option<String>,
}

#[derive(Deserialize)]
pub struct WebhookPaylaod {
    pub entry: Vec<Entry>,
}

#[derive(Deserialize)]
pub struct Entry {
    changes: Option<Vec<Changes>>,
}

#[derive(Deserialize)]
pub struct Changes {
    value: Option<Value>,
}

#[derive(Deserialize)]
pub struct Value {
    messages: Option<Vec<Messages>>,
}

#[derive(Deserialize)]
pub struct Messages {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<Text>,
}

#[derive(Deserialize)]
pub struct Text {
    body: Option<String>,
}

pub async fn wa_verify(Query(p): Query<VerifyParams>) -> impl IntoResponse {
    let verify_key = std::env::var("WA_VERIFY_KEY").expect("WA_VERIFY_KEY not set");
    if p.mode.as_deref() == Some("subscribe") && p.token.as_deref() == Some(verify_key.as_str()) {
        (
            StatusCode::OK,
            p.challenge.expect("challenge not found in query"),
        )
    } else {
        (StatusCode::FORBIDDEN, String::new())
    }
}

fn valid_signature(headers: &HeaderMap, body: &str) -> bool {
    let Ok(secret) = std::env::var("META_APP_SECRET") else {
        return false;
    };
    let Some(header) = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(hex_sig) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(their_sig) = hex::decode(hex_sig) else {
        return false;
    };

    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(body.as_bytes());
    mac.verify_slice(&their_sig).is_ok()
}

pub async fn wa_receive(headers: HeaderMap, body: String) -> impl IntoResponse {
    if !valid_signature(&headers, &body) {
        eprintln!("wa: rejected unsigned/invalid request");
        return StatusCode::FORBIDDEN;
    }
    match serde_json::from_str::<WebhookPaylaod>(&body) {
        Ok(p) => {
            for entry in p.entry {
                for change in entry.changes.into_iter().flatten() {
                    let Some(value) = change.value else { continue };
                    for message in value.messages.into_iter().flatten() {
                        if message.kind.as_deref() == Some("text") {
                            let Some(text) = message.text else { continue };
                            println!("{:?}", text.body);
                        }
                    }
                }
            }
            StatusCode::OK
        }
        Err(_e) => StatusCode::OK,
    }
}
