/**
* this file code contains tests for desktop channel protocol, authentication, and session handling
*/
use axum::body::Body;
use axum::http::{Request, StatusCode};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::util::ServiceExt;

use vox_bridge::app::build_router;
use vox_bridge::channels::desktop::protocol::{
    DesktopInboundText, DesktopOutboundText, parse_inbound_text, serialize_outbound_text,
};
use vox_bridge::channels::desktop::session::{
    CreateDesktopSessionRequest, CreateDesktopSessionResponse, DesktopSessionState,
};
use vox_bridge::core::client::CoreClient;
use vox_bridge::state::AppState;
use vox_bridge::voice::config::VoiceConfig;
use vox_bridge::voice::registry::VoiceRuntime;

fn create_test_state() -> Arc<AppState> {
    create_test_state_with_core("http://127.0.0.1:3001")
}

const ALICE: &str = "7d7f3a52-1f3c-4b8e-9a53-2f0e7c1a9b10";

/// Minimal stand-in for Core's `/v1/me`: accepts only `Bearer alice-token`.
async fn spawn_mock_core() -> String {
    use axum::{Json, Router, http::HeaderMap, routing::get};
    let app = Router::new().route(
        "/v1/me",
        get(|headers: HeaderMap| async move {
            match headers.get("authorization").and_then(|v| v.to_str().ok()) {
                Some("Bearer alice-token") => {
                    Ok(Json(serde_json::json!({ "user_id": ALICE })))
                }
                _ => Err(StatusCode::UNAUTHORIZED),
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn create_test_state_with_core(core_url: &str) -> Arc<AppState> {
    let twilio = Arc::new(DashMap::new());
    let config = VoiceConfig::from_values(|k| match k {
        "ASSEMBLYAI_API_KEY" => Some("test-key".into()),
        "VOX_CORE_URL" => Some("http://127.0.0.1:3001".into()),
        "VOX_AUTH_TOKEN" => Some("test-core-token".into()),
        "ELEVENLABS_API_KEY" => Some("test-el-key".into()),
        _ => None,
    })
    .unwrap();

    let voice = Arc::new(VoiceRuntime::from_config(config).unwrap());
    let core_client = Arc::new(
        CoreClient::new("http://127.0.0.1:3001".into(), "test-core-token".into()).unwrap(),
    );

    Arc::new(AppState {
        twilio,
        twilio_account_sid: Arc::new("AC123".into()),
        twilio_auth_token: Arc::new("secret".into()),
        twilio_from_number: Arc::new("+1234567890".into()),
        service_token: Arc::new("test-core-token".into()),
        core_url: Arc::new(core_url.into()),
        telephony: None,
        voice,
        core_client,
        whatsapp_verify_token: None,
        whatsapp_app_secret: None,
        whatsapp_access_token: None,
        whatsapp_phone_id: None,
        desktop_sessions: Arc::new(DashMap::new()),
        opt_outs: Arc::new(DashMap::new()),
        notification_deliveries: Arc::new(DashMap::new()),
        messaging_client: None,
    })
}

fn ws_request(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap()
}

#[test]
fn test_desktop_protocol_serialization() {
    let inbound_start = DesktopInboundText::Start {
        client_version: Some("0.1.0".into()),
    };
    let json_start = serde_json::to_string(&inbound_start).unwrap();
    assert!(json_start.contains(r#""event":"start""#));
    assert!(json_start.contains(r#""client_version":"0.1.0""#));

    let parsed_start = parse_inbound_text(&json_start).unwrap();
    assert_eq!(parsed_start, inbound_start);

    let inbound_mark = DesktopInboundText::Mark {
        name: "response-1".into(),
    };
    let json_mark = serde_json::to_string(&inbound_mark).unwrap();
    assert!(json_mark.contains(r#""event":"mark""#));
    assert!(json_mark.contains(r#""name":"response-1""#));

    let inbound_stop = DesktopInboundText::Stop;
    let json_stop = serde_json::to_string(&inbound_stop).unwrap();
    assert_eq!(json_stop, r#"{"event":"stop"}"#);

    let outbound_connected = DesktopOutboundText::Connected {
        session_id: "conv-123".into(),
        sample_rate: 8000,
        format: "pcm_mulaw".into(),
    };
    let json_conn = serialize_outbound_text(&outbound_connected).unwrap();
    assert!(json_conn.contains(r#""event":"connected""#));
    assert!(json_conn.contains(r#""session_id":"conv-123""#));

    let outbound_clear = DesktopOutboundText::Clear;
    let json_clear = serialize_outbound_text(&outbound_clear).unwrap();
    assert_eq!(json_clear, r#"{"event":"clear"}"#);
}

#[tokio::test]
async fn test_desktop_session_unauthorized_without_token() {
    let state = create_test_state();
    let app = build_router(state);

    let req_body = serde_json::to_vec(&CreateDesktopSessionRequest {
        external_conversation_id: None,
        opening_instruction: None,
    })
    .unwrap();

    let req = Request::post("/bridge/desktop/voice/session")
        .header("content-type", "application/json")
        .body(Body::from(req_body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_desktop_session_rejects_token_core_does_not_accept() {
    let core = spawn_mock_core().await;
    let app = build_router(create_test_state_with_core(&core));

    let req = Request::post("/bridge/desktop/voice/session")
        .header("content-type", "application/json")
        // The Core service token must not be able to mint sessions for anyone.
        .header("authorization", "Bearer test-core-token")
        .body(Body::from(r#"{"host_user_id":"vox-account:someone-else"}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_desktop_session_owner_comes_from_verified_token() {
    let core = spawn_mock_core().await;
    let state = create_test_state_with_core(&core);
    let app = build_router(state.clone());

    // A client-supplied host_user_id is ignored.
    let req_body = serde_json::json!({
        "host_user_id": "vox-account:someone-else",
        "external_conversation_id": "conv-alice-42",
        "opening_instruction": "Hello Alice",
    })
    .to_string();

    let req = Request::post("/bridge/desktop/voice/session")
        .header("content-type", "application/json")
        .header("authorization", "Bearer alice-token")
        .body(Body::from(req_body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 16)
        .await
        .unwrap();
    let session_resp: CreateDesktopSessionResponse = serde_json::from_slice(&body_bytes).unwrap();

    assert!(!session_resp.ticket.is_empty());
    assert_eq!(
        session_resp.stream_url,
        format!(
            "/bridge/desktop/voice/stream?ticket={}",
            session_resp.ticket
        )
    );
    assert_eq!(session_resp.expires_in_seconds, 60);

    let session = state.desktop_sessions.get(&session_resp.ticket).unwrap();
    assert_eq!(session.host_user_id, format!("vox-account:{ALICE}"));
    assert_eq!(session.external_conversation_id, "conv-alice-42");
    assert_eq!(session.opening_instruction.as_deref(), Some("Hello Alice"));
}

#[tokio::test]
async fn test_desktop_stream_ticket_anti_replay_and_expiry() {
    let state = create_test_state();

    // 1. Invalid ticket
    let app = build_router(state.clone());
    let req = ws_request("/bridge/desktop/voice/stream?ticket=nonexistent");
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // 2. Expired ticket
    let expired_ticket = "expired-ticket-123".to_string();
    state.desktop_sessions.insert(
        expired_ticket.clone(),
        DesktopSessionState {
            ticket: expired_ticket.clone(),
            host_user_id: "user-test".into(),
            external_conversation_id: "conv-test".into(),
            opening_instruction: None,
            expires_at: Instant::now() - Duration::from_secs(10),
        },
    );

    let app = build_router(state.clone());
    let req = ws_request(&format!(
        "/bridge/desktop/voice/stream?ticket={expired_ticket}"
    ));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Verify expired ticket was consumed and cannot be reused
    assert!(!state.desktop_sessions.contains_key(&expired_ticket));

    // 3. Valid ticket consumption (anti-replay)
    let valid_ticket = "valid-ticket-456".to_string();
    state.desktop_sessions.insert(
        valid_ticket.clone(),
        DesktopSessionState {
            ticket: valid_ticket.clone(),
            host_user_id: "user-test".into(),
            external_conversation_id: "conv-test".into(),
            opening_instruction: None,
            expires_at: Instant::now() + Duration::from_secs(60),
        },
    );

    // Consume once (in oneshot mode without live TCP socket, the ticket is validated & popped, returning upgrade rejection 426)
    let app = build_router(state.clone());
    let req = ws_request(&format!(
        "/bridge/desktop/voice/stream?ticket={valid_ticket}"
    ));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UPGRADE_REQUIRED);

    // The ticket must now be completely gone from state (single-use anti-replay)
    assert!(!state.desktop_sessions.contains_key(&valid_ticket));

    // Replay attempt must fail with 401 Unauthorized because ticket was already consumed
    let app2 = build_router(state.clone());
    let replay_req = ws_request(&format!(
        "/bridge/desktop/voice/stream?ticket={valid_ticket}"
    ));
    let replay_resp = app2.oneshot(replay_req).await.unwrap();
    assert_eq!(replay_resp.status(), StatusCode::UNAUTHORIZED);
}
