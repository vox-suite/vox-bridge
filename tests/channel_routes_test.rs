/**
* this file code contains tests for application routing and endpoints
*/
use axum::body::Body;
use axum::http::{Request, StatusCode};
use dashmap::DashMap;
use std::sync::Arc;
use tower::util::ServiceExt;

use vox_bridge::app::build_router;
use vox_bridge::core::client::CoreClient;
use vox_bridge::state::AppState;
use vox_bridge::voice::config::VoiceConfig;
use vox_bridge::voice::registry::VoiceRuntime;

#[tokio::test]
async fn health_check_returns_ok() {
    let twilio = Arc::new(DashMap::new());
    let config = VoiceConfig::from_values(|k| match k {
        "ASSEMBLYAI_API_KEY" => Some("key".into()),
        "VOX_CORE_URL" => Some("http://127.0.0.1:3001".into()),
        "VOX_AUTH_TOKEN" => Some("svc-token".into()),
        "ELEVENLABS_API_KEY" => Some("el-key".into()),
        _ => None,
    })
    .unwrap();

    let voice = Arc::new(VoiceRuntime::from_config(config).unwrap());
    let core_client =
        Arc::new(CoreClient::new("http://127.0.0.1:3001".into(), "svc-token".into()).unwrap());

    let state = Arc::new(AppState {
        twilio,
        twilio_account_sid: Arc::new("AC123".into()),
        twilio_auth_token: Arc::new("secret".into()),
        twilio_from_number: Arc::new("+1234567890".into()),
        service_token: Arc::new("svc-token".into()),
        core_url: Arc::new("http://127.0.0.1:3001".into()),
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
    });

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
