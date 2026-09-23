/**
* this file code contains outbound call dispatch handling
*/
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

use crate::channels::twilio::webhook::TwilioState;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct OutboundCallPayload {
    pub action_id: Uuid,
    pub identity: ChannelIdentityPayload,
    pub reason: String,
    pub opening_instruction: String,
    pub conversation_id: Uuid,
}

#[derive(Deserialize)]
pub struct ChannelIdentityPayload {
    pub channel: String,
    pub external_id: String,
}

#[derive(Serialize)]
pub struct OutboundCallResult {
    pub provider_call_id: String,
}

pub async fn handle_outbound_call(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<OutboundCallPayload>,
) -> Response {
    let auth_valid = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|token| {
            subtle::ConstantTimeEq::ct_eq(token.as_bytes(), state.service_token.as_bytes()).into()
        })
        .unwrap_or(false);

    if !auth_valid {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
    if payload.identity.channel != "phone"
        || payload.identity.external_id.trim().is_empty()
        || payload.reason.trim().is_empty()
        || payload.opening_instruction.trim().is_empty()
    {
        return (StatusCode::BAD_REQUEST, "Invalid outbound call request").into_response();
    }

    let action_str = payload.action_id.to_string();

    for entry in state.twilio.iter() {
        if entry.value().action_id.as_deref() == Some(&action_str) {
            return (
                StatusCode::OK,
                Json(OutboundCallResult {
                    provider_call_id: entry.value().call_sid.clone(),
                }),
            )
                .into_response();
        }
    }

    let Some(telephony) = state.telephony.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Telephony provider not configured",
        )
            .into_response();
    };

    match telephony
        .initiate_call(
            &payload.identity.external_id,
            payload.action_id,
            payload.conversation_id,
            Some(&payload.opening_instruction),
        )
        .await
    {
        Ok(call_sid) => {
            state.twilio.insert(
                call_sid.clone(),
                TwilioState {
                    call_sid: call_sid.clone(),
                    account_sid: state.twilio_account_sid.to_string(),
                    from: payload.identity.external_id,
                    to: state.twilio_from_number.to_string(),
                    call_status: Some("in_progress".into()),
                    opening_instruction: Some(payload.opening_instruction),
                    action_id: Some(action_str),
                    external_conversation_id: payload.conversation_id.to_string(),
                },
            );

            (
                StatusCode::OK,
                Json(OutboundCallResult {
                    provider_call_id: call_sid,
                }),
            )
                .into_response()
        }
        Err(err) => {
            tracing::warn!(error = %err, "Failed to initiate outbound call");
            (
                StatusCode::BAD_GATEWAY,
                "Failed to initiate call via telephony provider",
            )
                .into_response()
        }
    }
}
