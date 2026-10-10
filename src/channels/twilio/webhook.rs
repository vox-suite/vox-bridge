/**
* this file code contains twilio voice webhook handler
*/
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::channels::context::normalized_e164;
use crate::channels::twilio::signature::validate_twilio_signature;
use crate::state::AppState;

pub const VOICE_WEBHOOK_URL: &str = "https://api.callvox.in/bridge/twilio/voice";
pub const VOICE_STREAM_URL: &str = "wss://api.callvox.in/bridge/twilio/voice/stream";

#[derive(Clone, Deserialize, Serialize)]
pub struct TwilioState {
    #[serde(skip)]
    pub accepted_at: Option<std::time::Instant>,
    pub call_sid: String,
    pub account_sid: String,
    pub from: String,
    pub to: String,
    pub call_status: Option<String>,
    pub opening_instruction: Option<String>,
    pub action_id: Option<String>,
    pub external_conversation_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct TwilioPostParams {
    pub call_sid: String,
    pub account_sid: String,
    pub from: String,
    pub to: String,
    pub call_status: Option<String>,
}

pub async fn initialize_voice_socket(
    State(app_state): State<Arc<AppState>>,
    header: HeaderMap,
    body: String,
) -> Response {
    let accepted_at = std::time::Instant::now();
    let Some(signature) = header
        .get("x-twilio-signature")
        .and_then(|value| value.to_str().ok())
    else {
        tracing::warn!("rejected Twilio webhook without a valid signature header");
        return (StatusCode::UNAUTHORIZED, "Missing Twilio signature").into_response();
    };

    let all_parameters: Vec<(String, String)> = match serde_urlencoded::from_str(&body) {
        Ok(parameters) => parameters,
        Err(error) => {
            tracing::warn!(%error, "invalid Twilio webhook form body");
            return (StatusCode::BAD_REQUEST, "Invalid form body").into_response();
        }
    };

    let params: TwilioPostParams = match serde_urlencoded::from_str(&body) {
        Ok(params) => params,
        Err(error) => {
            tracing::warn!(%error, "Twilio webhook is missing required call parameters");
            return (StatusCode::BAD_REQUEST, "Invalid form body").into_response();
        }
    };

    if !validate_twilio_signature(
        app_state.twilio_auth_token.as_str(),
        VOICE_WEBHOOK_URL,
        &all_parameters,
        signature,
    ) {
        tracing::warn!("rejected Twilio webhook with an invalid signature");
        return (StatusCode::FORBIDDEN, "Invalid Twilio signature").into_response();
    }

    let Some(from) = normalized_e164(&params.from) else {
        tracing::warn!("rejected Twilio webhook with malformed caller identity");
        return (StatusCode::BAD_REQUEST, "Invalid caller identity").into_response();
    };

    let call_sid = params.call_sid.clone();
    let external_conversation_id = params.call_sid.clone();
    tracing::info!(%call_sid, conversation_id = %external_conversation_id, validation_ms = accepted_at.elapsed().as_millis() as u64, "VOICE_WEBHOOK_ACCEPTED");
    app_state.twilio.insert(
        params.call_sid.clone(),
        TwilioState {
            accepted_at: Some(accepted_at),
            call_sid: params.call_sid,
            account_sid: params.account_sid,
            from,
            to: params.to,
            call_status: params.call_status,
            opening_instruction: Some("The call just connected. Greet the user.".into()),
            action_id: None,
            external_conversation_id,
        },
    );

    tracing::info!(%call_sid, "accepted incoming Twilio call");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml")],
        voice_twiml(),
    )
        .into_response()
}

pub fn voice_twiml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Connect>
    <Stream url="{VOICE_STREAM_URL}" />
  </Connect>
</Response>"#
    )
}
