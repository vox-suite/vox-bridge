/**
* this file code contains tests for the outbound call dispatch endpoint
*/
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use dashmap::DashMap;
use std::sync::{Arc, Mutex};
use tower::util::ServiceExt;
use uuid::Uuid;

use vox_bridge::app::build_router;
use vox_bridge::core::client::CoreClient;
use vox_bridge::providers::telephony::{TelephonyClient, TelephonyError};
use vox_bridge::state::AppState;
use vox_bridge::voice::config::VoiceConfig;
use vox_bridge::voice::registry::VoiceRuntime;

#[derive(Default)]
struct RecordingTelephony {
    dialed: Mutex<Vec<String>>,
}

#[async_trait]
impl TelephonyClient for RecordingTelephony {
    async fn initiate_call(
        &self,
        to: &str,
        _action_id: Uuid,
        _conversation_id: Uuid,
        _opening_instruction: Option<&str>,
    ) -> Result<String, TelephonyError> {
        self.dialed.lock().unwrap().push(to.to_string());
        Ok("CA_test_sid".into())
    }
}

fn app_with(telephony: Option<Arc<RecordingTelephony>>) -> axum::Router {
    let config = VoiceConfig::from_values(|k| match k {
        "ASSEMBLYAI_API_KEY" => Some("key".into()),
        "VOX_CORE_URL" => Some("http://127.0.0.1:3001".into()),
        "VOX_AUTH_TOKEN" => Some("svc-token".into()),
        "ELEVENLABS_API_KEY" => Some("el-key".into()),
        _ => None,
    })
    .unwrap();

    let state = Arc::new(AppState {
        twilio: Arc::new(DashMap::new()),
        twilio_account_sid: Arc::new("AC123".into()),
        twilio_auth_token: Arc::new("secret".into()),
        twilio_from_number: Arc::new("+1234567890".into()),
        service_token: Arc::new("svc-token".into()),
        core_url: Arc::new("http://127.0.0.1:3001".into()),
        telephony: telephony.map(|t| t as Arc<dyn TelephonyClient>),
        voice: Arc::new(VoiceRuntime::from_config(config).unwrap()),
        core_client: Arc::new(
            CoreClient::new("http://127.0.0.1:3001".into(), "svc-token".into()).unwrap(),
        ),
        whatsapp_verify_token: None,
        whatsapp_app_secret: None,
        whatsapp_access_token: None,
        whatsapp_phone_id: None,
        desktop_sessions: Arc::new(DashMap::new()),
        opt_outs: Arc::new(DashMap::new()),
        notification_deliveries: Arc::new(DashMap::new()),
        messaging_client: None,
    });
    build_router(state)
}

fn request(token: Option<&str>, action_id: Uuid, external_id: &str) -> Request<Body> {
    let body = serde_json::json!({
        "action_id": action_id,
        "identity": { "channel": "phone", "external_id": external_id },
        "reason": "Reminder",
        "opening_instruction": "Remind the user to stretch",
        "conversation_id": Uuid::new_v4(),
    });
    let mut builder = Request::builder()
        .method("POST")
        .uri("/internal/v1/actions/outbound-call")
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn rejects_missing_or_wrong_service_token() {
    let telephony = Arc::new(RecordingTelephony::default());
    let app = app_with(Some(telephony.clone()));

    for token in [None, Some("wrong")] {
        let response = app
            .clone()
            .oneshot(request(token, Uuid::new_v4(), "+15551234567"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    assert!(telephony.dialed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dials_digits_only_number_as_e164() {
    let telephony = Arc::new(RecordingTelephony::default());
    let app = app_with(Some(telephony.clone()));

    let response = app
        .oneshot(request(Some("svc-token"), Uuid::new_v4(), "15551234567"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(*telephony.dialed.lock().unwrap(), vec!["+15551234567"]);
}

#[tokio::test]
async fn dials_formatted_number_as_e164() {
    let telephony = Arc::new(RecordingTelephony::default());
    let app = app_with(Some(telephony.clone()));

    let response = app
        .oneshot(request(
            Some("svc-token"),
            Uuid::new_v4(),
            " +1 (555) 123-4567 ",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(*telephony.dialed.lock().unwrap(), vec!["+15551234567"]);
}

#[tokio::test]
async fn rejects_unusable_phone_number_without_dialing() {
    let telephony = Arc::new(RecordingTelephony::default());
    let app = app_with(Some(telephony.clone()));

    for bad in ["", "abc", "12345"] {
        let response = app
            .clone()
            .oneshot(request(Some("svc-token"), Uuid::new_v4(), bad))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "input {bad:?}");
    }
    assert!(telephony.dialed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn retry_with_same_action_id_does_not_dial_twice() {
    let telephony = Arc::new(RecordingTelephony::default());
    let app = app_with(Some(telephony.clone()));
    let action_id = Uuid::new_v4();

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(request(Some("svc-token"), action_id, "15551234567"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(telephony.dialed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn reports_unavailable_when_telephony_not_configured() {
    let app = app_with(None);

    let response = app
        .oneshot(request(Some("svc-token"), Uuid::new_v4(), "15551234567"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
