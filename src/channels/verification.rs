use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};

use crate::retry::retry_with_backoff;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct VerificationCodePayload {
    pub phone_number: String,
    pub code: String,
}

fn all_digits(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
}

pub async fn handle_verification_code(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<VerificationCodePayload>,
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
    if !all_digits(&payload.phone_number, 7, 15) || !all_digits(&payload.code, 4, 8) {
        return (StatusCode::BAD_REQUEST, "Invalid verification request").into_response();
    }

    let from = std::env::var("TWILIO_WHATSAPP_FROM")
        .ok()
        .map(|value| {
            value
                .trim()
                .trim_start_matches("whatsapp:")
                .trim_start_matches('+')
                .to_string()
        })
        .filter(|value| all_digits(value, 7, 15));
    let content_sid = std::env::var("TWILIO_OTP_CONTENT_SID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| value.starts_with("HX"));
    let (Some(from), Some(content_sid)) = (from, content_sid) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "WhatsApp verification is not configured",
        )
            .into_response();
    };

    let url = format!(
        "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
        state.twilio_account_sid
    );
    let variables = serde_json::json!({ "1": payload.code }).to_string();
    let form = [
        ("To", format!("whatsapp:+{}", payload.phone_number)),
        ("From", format!("whatsapp:+{from}")),
        ("ContentSid", content_sid),
        ("ContentVariables", variables),
    ];
    let client = reqwest::Client::new();
    let result = retry_with_backoff(2, Duration::from_millis(300), || async {
        let response = client
            .post(&url)
            .basic_auth(
                state.twilio_account_sid.as_str(),
                Some(state.twilio_auth_token.as_str()),
            )
            .timeout(Duration::from_secs(10))
            .form(&form)
            .send()
            .await?;
        if let Err(err) = response.error_for_status_ref() {
            tracing::warn!(status = %response.status(), "Twilio WhatsApp verification send failed");
            return Err(err);
        }
        Ok(())
    })
    .await;
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "Verification message failed").into_response(),
    }
}
