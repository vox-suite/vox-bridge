/**
* this file code contains twilio call status callback handler
*/
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::channels::twilio::signature::validate_twilio_signature;
use crate::state::AppState;

pub const VOICE_STATUS_URL: &str = "https://api.voxagent.in/bridge/twilio/voice/status";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct TwilioStatusParams {
    pub call_sid: String,
    pub call_status: Option<String>,
}

pub async fn handle_voice_status(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Some(signature) = headers
        .get("x-twilio-signature")
        .and_then(|value| value.to_str().ok())
    else {
        tracing::warn!("rejected Twilio status callback without signature");
        return (StatusCode::UNAUTHORIZED, "Missing Twilio signature").into_response();
    };

    let all_parameters: Vec<(String, String)> = match serde_urlencoded::from_str(&body) {
        Ok(parameters) => parameters,
        Err(error) => {
            tracing::warn!(%error, "invalid Twilio status callback form body");
            return (StatusCode::BAD_REQUEST, "Invalid form body").into_response();
        }
    };

    let params: TwilioStatusParams = match serde_urlencoded::from_str(&body) {
        Ok(params) => params,
        Err(error) => {
            tracing::warn!(%error, "Twilio status callback missing required fields");
            return (StatusCode::BAD_REQUEST, "Invalid form body").into_response();
        }
    };

    if !validate_twilio_signature(
        app_state.twilio_auth_token.as_str(),
        VOICE_STATUS_URL,
        &all_parameters,
        signature,
    ) {
        tracing::warn!("rejected Twilio status callback with invalid signature");
        return (StatusCode::FORBIDDEN, "Invalid Twilio signature").into_response();
    }

    if let Some(status) = params.call_status {
        tracing::info!(call_sid = %params.call_sid, %status, "Twilio call status update");
        if matches!(
            status.as_str(),
            "completed" | "failed" | "busy" | "no-answer" | "canceled"
        ) {
            app_state.twilio.remove(&params.call_sid);
        }
    }

    StatusCode::OK.into_response()
}
