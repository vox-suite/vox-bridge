use crate::AppState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;

pub(crate) const VOICE_STATUS_URL: &str = "https://api.voxagent.in/bridge/twilio/voice/status";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TwilioStatusParams {
    pub call_sid: String,
    pub call_status: String,
}

pub async fn handle_voice_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Some(signature) = headers
        .get("x-twilio-signature")
        .and_then(|value| value.to_str().ok())
    else {
        return (StatusCode::UNAUTHORIZED, "Missing Twilio signature").into_response();
    };

    let all_parameters: Vec<(String, String)> = match serde_urlencoded::from_str(&body) {
        Ok(parameters) => parameters,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid form body").into_response(),
    };

    if !crate::routes::twilio::twilio_post::validate_twilio_signature(
        state.twilio_auth_token.as_str(),
        VOICE_STATUS_URL,
        &all_parameters,
        signature,
    ) {
        return (StatusCode::FORBIDDEN, "Invalid Twilio signature").into_response();
    }

    let params: TwilioStatusParams = match serde_urlencoded::from_str(&body) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid parameters").into_response(),
    };

    let action_id = state
        .twilio
        .get(&params.call_sid)
        .and_then(|entry| entry.action_id.clone());

    if let Some(action_id) = action_id {
        let status = if params.call_status == "completed" {
            "succeeded"
        } else {
            "failed"
        };

        let client = reqwest::Client::new();
        let endpoint = format!(
            "{}/v1/actions/{action_id}/result",
            state.core_url.trim_end_matches('/')
        );
        let _ = client
            .post(&endpoint)
            .bearer_auth(state.service_token.as_str())
            .json(&serde_json::json!({
                "status": status,
                "provider_call_id": params.call_sid,
                "error_code": None::<String>
            }))
            .send()
            .await;
    }

    state.twilio.remove(&params.call_sid);
    StatusCode::OK.into_response()
}
