use crate::AppState;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

pub(crate) const VOICE_STATUS_URL: &str = "https://api.voxagent.in/bridge/twilio/voice/status";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TwilioStatusParams {
    pub call_sid: String,
    pub call_status: String,
}

#[derive(Deserialize)]
pub struct ActionQuery {
    pub action_id: Uuid,
}

pub async fn handle_voice_status(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ActionQuery>,
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

    let callback_url = format!("{VOICE_STATUS_URL}?action_id={}", query.action_id);
    if !crate::routes::twilio::twilio_post::validate_twilio_signature(
        state.twilio_auth_token.as_str(),
        &callback_url,
        &all_parameters,
        signature,
    ) {
        return (StatusCode::FORBIDDEN, "Invalid Twilio signature").into_response();
    }

    let params: TwilioStatusParams = match serde_urlencoded::from_str(&body) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "Invalid parameters").into_response(),
    };

    let status = if params.call_status == "completed" {
        "succeeded"
    } else {
        "failed"
    };
    let client = reqwest::Client::new();
    let endpoint = format!(
        "{}/v1/actions/{}/result",
        state.core_url.trim_end_matches('/'),
        query.action_id
    );
    let response = client
        .post(&endpoint)
        .bearer_auth(state.service_token.as_str())
        .json(&serde_json::json!({
            "status": status,
            "provider_call_id": params.call_sid,
            "error_code": (status == "failed").then_some(params.call_status.clone())
        }))
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => {}
        Ok(response) => {
            tracing::warn!(status = %response.status(), action_id = %query.action_id, "Core rejected action callback");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        Err(error) => {
            tracing::warn!(%error, action_id = %query.action_id, "Core action callback failed");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    }

    state.twilio.remove(&params.call_sid);
    StatusCode::OK.into_response()
}
