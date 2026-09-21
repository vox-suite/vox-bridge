use crate::{AppState, voice::context::CallContext};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::sync::Arc;

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
pub struct Contact {
    profile: Option<Profile>,
    #[allow(dead_code)]
    wa_id: Option<String>,
}

#[derive(Deserialize)]
pub struct Profile {
    name: Option<String>,
}

#[derive(Deserialize)]
pub struct Value {
    contacts: Option<Vec<Contact>>,
    messages: Option<Vec<Messages>>,
}

#[derive(Deserialize)]
pub struct Messages {
    #[serde(rename = "type")]
    kind: Option<String>,
    from: Option<String>,
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

pub async fn wa_receive(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if !valid_signature(&headers, &body) {
        eprintln!("wa: rejected unsigned/invalid request");
        return StatusCode::FORBIDDEN;
    }
    match serde_json::from_str::<WebhookPaylaod>(&body) {
        Ok(p) => {
            for entry in p.entry {
                for change in entry.changes.into_iter().flatten() {
                    let Some(value) = change.value else { continue };

                    // Extract user profile display name from Meta contacts if available
                    let profile_name = value
                        .contacts
                        .as_ref()
                        .and_then(|c| c.first())
                        .and_then(|c| c.profile.as_ref())
                        .and_then(|p| p.name.clone());

                    for message in value.messages.into_iter().flatten() {
                        if message.kind.as_deref() == Some("text") {
                            let Some(text) = message.text else { continue };
                            let Some(body) = text.body else { continue };
                            let Some(from) = message.from else { continue };
                            let profile = state.voice.resolver.resolve();
                            let Ok(providers) = state.voice.providers.providers_for(&profile)
                            else {
                                continue;
                            };
                            let context = CallContext {
                                channel: "whatsapp".into(),
                                external_identity: from.clone(),
                                external_conversation_id: format!("whatsapp:{from}"),
                                initiation_context: profile_name
                                    .as_ref()
                                    .map(|n| format!("whatsapp_name:{n}")),
                                voice_signature: None,
                                turn_id: None,
                                revision: None,
                                tts_provider: None,
                            };
                            if let Ok(reply) = providers.agent.respond(&context, &body).await {
                                let _ = send_message(&reply, from.as_str()).await;
                            }
                        }
                    }
                }
            }
            StatusCode::OK
        }
        Err(_e) => StatusCode::OK,
    }
}

pub async fn send_message(msg: &str, receiver_number: &str) -> Result<(), reqwest::Error> {
    let token = std::env::var("WHATSAPP_ACCESS_KEY").expect("WHATSAPP_ACCESS_KEY not set");
    let phone_id = std::env::var("WHATSAPP_PHONE_ID").expect("WHATSAPP_PHONE_ID not set");
    let url = format!("https://graph.facebook.com/v25.0/{phone_id}/messages");
    let response = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "messaging_product": "whatsapp",
            "to": receiver_number,
            "type": "text",
            "text": {
                "body": msg
            }
        }))
        .send()
        .await?;

    if !response.status().is_success() {
        eprintln!("WA send message fail: {}", response.status());
    }
    Ok(())
}
