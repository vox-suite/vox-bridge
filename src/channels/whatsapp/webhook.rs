/**
* this file code contains whatsapp webhook verification and receipt handlers
*/
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::sync::Arc;

use crate::channels::context::{CallContext, normalized_e164};
use crate::channels::whatsapp::client::send_whatsapp_message;
use crate::core::ConversationClient;
use crate::state::AppState;

type HmacSha256 = Hmac<Sha256>;

#[derive(Deserialize)]
pub struct VerifyParams {
    #[serde(rename = "hub.mode")]
    pub mode: Option<String>,
    #[serde(rename = "hub.challenge")]
    pub challenge: Option<String>,
    #[serde(rename = "hub.verify_token")]
    pub token: Option<String>,
}

#[derive(Deserialize)]
pub struct WebhookPayload {
    pub entry: Vec<Entry>,
}

#[derive(Deserialize)]
pub struct Entry {
    pub changes: Option<Vec<Changes>>,
}

#[derive(Deserialize)]
pub struct Changes {
    pub value: Option<Value>,
}

#[derive(Deserialize)]
pub struct Contact {
    pub profile: Option<Profile>,
    pub wa_id: Option<String>,
}

#[derive(Deserialize)]
pub struct Profile {
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct Value {
    pub contacts: Option<Vec<Contact>>,
    pub messages: Option<Vec<Messages>>,
}

#[derive(Deserialize)]
pub struct Messages {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub from: Option<String>,
    pub text: Option<TextPayload>,
}

#[derive(Deserialize)]
pub struct TextPayload {
    pub body: Option<String>,
}

pub async fn wa_verify(
    State(state): State<Arc<AppState>>,
    Query(p): Query<VerifyParams>,
) -> impl IntoResponse {
    let verify_key = state.whatsapp_verify_token.clone().unwrap_or_default();

    if p.mode.as_deref() == Some("subscribe") && p.token.as_deref() == Some(verify_key.as_str()) {
        (StatusCode::OK, p.challenge.unwrap_or_default())
    } else {
        (StatusCode::FORBIDDEN, String::new())
    }
}

pub fn valid_signature(secret: &str, headers: &HeaderMap, body: &str) -> bool {
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

    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body.as_bytes());
    mac.verify_slice(&their_sig).is_ok()
}

pub async fn wa_receive(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let secret = state.whatsapp_app_secret.clone().unwrap_or_default();

    if !valid_signature(&secret, &headers, &body) {
        tracing::warn!("wa: rejected unsigned/invalid request");
        return StatusCode::FORBIDDEN;
    }

    match serde_json::from_str::<WebhookPayload>(&body) {
        Ok(p) => {
            for entry in p.entry {
                for change in entry.changes.into_iter().flatten() {
                    let Some(value) = change.value else { continue };

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
                            let Some(from) = normalized_e164(&from) else {
                                continue;
                            };
                            let context = CallContext {
                                channel: "whatsapp".into(),
                                external_identity: from.clone(),
                                external_conversation_id: format!("whatsapp:{from}"),
                                initiation_context: profile_name
                                    .as_ref()
                                    .map(|n| format!("whatsapp_name:{n}")),
                                turn_id: None,
                                revision: None,
                                tts_provider: None,
                                filler: None,
                            };
                            if crate::channels::notifications::is_opt_out_keyword(&body) {
                                let now = crate::channels::notifications::now_epoch_secs();
                                state.opt_outs.insert(
                                    format!("whatsapp:{from}"),
                                    crate::channels::notifications::OptOutRecord {
                                        destination: from.clone(),
                                        channel: "whatsapp".into(),
                                        opted_out_at: now,
                                        reason: "Opt-out keyword received".into(),
                                    },
                                );
                                state.opt_outs.insert(
                                    format!("all:{from}"),
                                    crate::channels::notifications::OptOutRecord {
                                        destination: from.clone(),
                                        channel: "all".into(),
                                        opted_out_at: now,
                                        reason: "Opt-out keyword received".into(),
                                    },
                                );
                                let reply = "You have unsubscribed from notifications. Reply START to resubscribe.".to_string();
                                if let (Some(token), Some(phone_id)) =
                                    (&state.whatsapp_access_token, &state.whatsapp_phone_id)
                                {
                                    let _ =
                                        send_whatsapp_message(token, phone_id, &from, &reply).await;
                                }
                                continue;
                            }
                            if crate::channels::notifications::is_opt_in_keyword(&body) {
                                state.opt_outs.remove(&format!("whatsapp:{from}"));
                                state.opt_outs.remove(&format!("all:{from}"));
                                let reply = "You have resubscribed to notifications.".to_string();
                                if let (Some(token), Some(phone_id)) =
                                    (&state.whatsapp_access_token, &state.whatsapp_phone_id)
                                {
                                    let _ =
                                        send_whatsapp_message(token, phone_id, &from, &reply).await;
                                }
                                continue;
                            }

                            let command =
                                crate::channels::interaction::parse_channel_command(&body);
                            let reply = match command {
                                crate::channels::interaction::ChannelCommand::Approve { proposal_id } => {
                                    match crate::channels::interaction::ChannelInteractionHandler::handle_approve(
                                        &state.core_client,
                                        &context,
                                        &from,
                                        proposal_id,
                                        serde_json::json!({}),
                                    )
                                    .await
                                    {
                                        Ok(msg) => msg,
                                        Err(err) => err.to_string(),
                                    }
                                }
                                crate::channels::interaction::ChannelCommand::Reject { proposal_id } => {
                                    format!("Proposal [{proposal_id}] was rejected. No action was executed.")
                                }
                                crate::channels::interaction::ChannelCommand::Status { task_id: Some(id) } => {
                                    match crate::channels::interaction::ChannelInteractionHandler::handle_status(
                                        &state.core_client,
                                        &context,
                                        &from,
                                        id,
                                    )
                                    .await
                                    {
                                        Ok(msg) => msg,
                                        Err(err) => err.to_string(),
                                    }
                                }
                                crate::channels::interaction::ChannelCommand::Status { task_id: None } => {
                                    "Please provide the task ID to check status: 'status <task-uuid>'".to_string()
                                }
                                crate::channels::interaction::ChannelCommand::Clarify { text, .. } => {
                                    let clarify_prompt = format!("User clarification: {text}");
                                    match state.core_client.respond(&context, &clarify_prompt).await {
                                        Ok(rep) => rep,
                                        Err(err) => crate::channels::interaction::UncertaintyNarrator::unconfirmed_outcome(
                                            "clarification submission",
                                            &err.to_string(),
                                        ),
                                    }
                                }
                                crate::channels::interaction::ChannelCommand::Text(_) => {
                                    match state.core_client.respond(&context, &body).await {
                                        Ok(rep) => rep,
                                        Err(err) => crate::channels::interaction::UncertaintyNarrator::unconfirmed_outcome(
                                            "your message",
                                            &err.to_string(),
                                        ),
                                    }
                                }
                            };

                            let token = state.whatsapp_access_token.clone().unwrap_or_default();
                            let phone_id = state.whatsapp_phone_id.clone().unwrap_or_default();
                            let _ = send_whatsapp_message(&token, &phone_id, from.as_str(), &reply)
                                .await;
                        }
                    }
                }
            }
            StatusCode::OK
        }
        Err(_) => StatusCode::OK,
    }
}
