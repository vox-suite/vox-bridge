/**
* this file code contains desktop session authentication and ticket management
*/
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::state::AppState;

pub const DEFAULT_TICKET_TTL_SECS: u64 = 60;

#[derive(Clone, Debug)]
pub struct DesktopSessionState {
    pub ticket: String,
    pub host_user_id: String,
    pub external_conversation_id: String,
    pub opening_instruction: Option<String>,
    pub expires_at: Instant,
}

/// The session owner always comes from the verified bearer token; any
/// client-supplied user id is ignored.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateDesktopSessionRequest {
    #[serde(default)]
    pub external_conversation_id: Option<String>,
    #[serde(default)]
    pub opening_instruction: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateDesktopSessionResponse {
    pub ticket: String,
    pub stream_url: String,
    pub expires_in_seconds: u64,
    pub host_user_id: String,
}

#[derive(Debug, Deserialize)]
struct CoreMeResponse {
    user_id: Uuid,
}

pub async fn create_desktop_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<CreateDesktopSessionRequest>,
) -> Response {
    let Some(host_user_id) = resolve_host_user_id(&state, &headers).await else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    };

    let ticket = Uuid::new_v4().to_string();
    let conversation_id = payload
        .external_conversation_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("desktop-conv-{}", Uuid::new_v4()));

    let expires_at = Instant::now() + Duration::from_secs(DEFAULT_TICKET_TTL_SECS);

    let session = DesktopSessionState {
        ticket: ticket.clone(),
        host_user_id: host_user_id.clone(),
        external_conversation_id: conversation_id,
        opening_instruction: payload.opening_instruction,
        expires_at,
    };

    let now = Instant::now();
    state.desktop_sessions.retain(|_, s| s.expires_at > now);
    state.desktop_sessions.insert(ticket.clone(), session);

    let stream_url = format!("/bridge/desktop/voice/stream?ticket={ticket}");
    (
        StatusCode::CREATED,
        Json(CreateDesktopSessionResponse {
            ticket,
            stream_url,
            expires_in_seconds: DEFAULT_TICKET_TTL_SECS,
            host_user_id,
        }),
    )
        .into_response()
}

async fn resolve_host_user_id(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let bearer = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())?;
    let user_id = verify_user_session(state, bearer).await?;
    Some(format!("vox-account:{user_id}"))
}

fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client")
    })
}

async fn verify_user_session(state: &AppState, bearer: &str) -> Option<Uuid> {
    let me_url = format!("{}/v1/me", state.core_url.trim_end_matches('/'));
    let response = http_client()
        .get(&me_url)
        .header("authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let parsed = response.json::<CoreMeResponse>().await.ok()?;
    Some(parsed.user_id)
}
