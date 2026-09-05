use axum::{extract::Query, http::StatusCode, response::IntoResponse};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct VerifyParams {
    #[serde(rename = "hub.mode")]
    mode: Option<String>,
    #[serde(rename = "hub.challenge")]
    challenge: Option<String>,
    #[serde(rename = "hub.verify_token")]
    token: Option<String>,
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
