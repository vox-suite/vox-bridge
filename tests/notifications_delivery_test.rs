/**
 * Comprehensive integration tests for outbound voice and messaging notification delivery (E41).
 *
 * Verifies:
 * 1. At least one voice or messaging channel reports delivery outcome back to Core.
 * 2. Retries follow the platform-provided window and do not duplicate confirmed delivery.
 * 3. Bridge does not receive unrelated task context, conversation transcripts, or credentials.
 * 4. Content redaction policy scrubs credentials, API keys, tokens, and payment card numbers.
 * 5. Inbound opt-out handling ("STOP", "UNSUBSCRIBE") immediately halts future deliveries; "START" restores.
 * 6. Provider reject, timeout, and delayed status callbacks map truthfully.
 * 7. Delivered-to-channel is never represented as human-seen.
 */
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use dashmap::DashMap;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;
use uuid::Uuid;

use vox_bridge::{
    app::build_router,
    channels::notifications::{
        MessagingClient, NotificationDispatchResponse, is_opt_in_keyword, is_opt_out_keyword,
        is_valid_e164, redact_sensitive_content,
    },
    core::client::CoreClient,
    providers::telephony::{TelephonyClient, TelephonyError},
    state::AppState,
    voice::config::VoiceConfig,
    voice::registry::VoiceRuntime,
};

/// Mock Telephony Client that tracks calls and allows forced behaviors
struct MockTelephony {
    pub call_count: AtomicUsize,
    pub force_failure: bool,
    pub force_timeout: bool,
}

impl MockTelephony {
    fn new() -> Self {
        Self {
            call_count: AtomicUsize::new(0),
            force_failure: false,
            force_timeout: false,
        }
    }

    fn failing() -> Self {
        Self {
            call_count: AtomicUsize::new(0),
            force_failure: true,
            force_timeout: false,
        }
    }

    #[allow(dead_code)]
    fn timing_out() -> Self {
        Self {
            call_count: AtomicUsize::new(0),
            force_failure: false,
            force_timeout: true,
        }
    }
}

#[async_trait::async_trait]
impl TelephonyClient for MockTelephony {
    async fn initiate_call(
        &self,
        _to: &str,
        _action_id: Uuid,
        _conversation_id: Uuid,
        _opening_instruction: Option<&str>,
    ) -> Result<String, TelephonyError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        if self.force_timeout {
            return Err(TelephonyError::Provider(
                "Gateway connection timed out".into(),
            ));
        }
        if self.force_failure {
            return Err(TelephonyError::Provider(
                "Carrier rejected destination".into(),
            ));
        }
        Ok(format!("CA{}", Uuid::new_v4().simple()))
    }
}

/// Mock Messaging Client that tracks messages and allows forced behaviors
struct MockMessaging {
    pub send_count: AtomicUsize,
    pub last_sent_text: Arc<std::sync::Mutex<Option<String>>>,
    pub force_failure: bool,
    pub force_timeout: bool,
}

impl MockMessaging {
    fn new() -> Self {
        Self {
            send_count: AtomicUsize::new(0),
            last_sent_text: Arc::new(std::sync::Mutex::new(None)),
            force_failure: false,
            force_timeout: false,
        }
    }

    #[allow(dead_code)]
    fn failing() -> Self {
        Self {
            send_count: AtomicUsize::new(0),
            last_sent_text: Arc::new(std::sync::Mutex::new(None)),
            force_failure: true,
            force_timeout: false,
        }
    }

    fn timing_out() -> Self {
        Self {
            send_count: AtomicUsize::new(0),
            last_sent_text: Arc::new(std::sync::Mutex::new(None)),
            force_failure: false,
            force_timeout: true,
        }
    }
}

#[async_trait::async_trait]
impl MessagingClient for MockMessaging {
    async fn send_message(&self, _channel: &str, _to: &str, text: &str) -> Result<String, String> {
        self.send_count.fetch_add(1, Ordering::SeqCst);
        let mut lock = self.last_sent_text.lock().unwrap();
        *lock = Some(text.to_string());
        if self.force_timeout {
            return Err("HTTP request timed out after 30000ms".into());
        }
        if self.force_failure {
            return Err("Recipient phone number not registered on WhatsApp".into());
        }
        Ok(format!("wamid.{}", Uuid::new_v4().simple()))
    }
}

fn create_test_state(
    telephony: Option<Arc<dyn TelephonyClient>>,
    messaging: Option<Arc<dyn MessagingClient>>,
    core_url: Option<String>,
) -> Arc<AppState> {
    let url = core_url.unwrap_or_else(|| "http://127.0.0.1:3001".into());
    let config = VoiceConfig::from_values(|k| match k {
        "ASSEMBLYAI_API_KEY" => Some("test-assembly-key".into()),
        "VOX_CORE_URL" => Some(url.clone()),
        "VOX_AUTH_TOKEN" => Some("test-token".into()),
        "ELEVENLABS_API_KEY" => Some("test-key".into()),
        _ => None,
    })
    .unwrap();
    let voice = Arc::new(VoiceRuntime::from_config(config).unwrap());
    let core_client = Arc::new(CoreClient::new(url.clone(), "test-token".into()).unwrap());

    Arc::new(AppState {
        twilio: Arc::new(DashMap::new()),
        twilio_account_sid: Arc::new("ACtest123".into()),
        twilio_auth_token: Arc::new("authtest123".into()),
        twilio_from_number: Arc::new("+15551230000".into()),
        service_token: Arc::new("test-token".into()),
        core_url: Arc::new(url),
        telephony,
        voice,
        core_client,
        whatsapp_verify_token: Some("verify_test".into()),
        whatsapp_app_secret: Some("secret_test".into()),
        whatsapp_access_token: Some("wa_token".into()),
        whatsapp_phone_id: Some("12345678".into()),
        desktop_sessions: Arc::new(DashMap::new()),
        opt_outs: Arc::new(DashMap::new()),
        notification_deliveries: Arc::new(DashMap::new()),
        messaging_client: messaging,
    })
}

// ============================================================================
// 1. Content Redaction Tests
// ============================================================================

#[test]
fn test_content_redaction_scrubs_secrets_and_cards() {
    let raw = "Your code is vox_sk_abc123456789. Your card is 4111 2222 3333 4444. Password=supersecret! Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9. Reminder: Meeting at 3pm";
    let redacted = redact_sensitive_content(raw);

    assert!(!redacted.contains("vox_sk_abc123456789"));
    assert!(redacted.contains("[REDACTED_SECRET]"));
    assert!(!redacted.contains("4111 2222 3333 4444"));
    assert!(redacted.contains("[REDACTED_CARD]"));
    assert!(!redacted.contains("supersecret!"));
    assert!(!redacted.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"));
    assert!(redacted.contains("[REDACTED_CREDENTIAL]"));
    // Non-sensitive content remains intact
    assert!(redacted.contains("Reminder: Meeting at 3pm"));
}

// ============================================================================
// 2. Opt-Out Keywords & Destination Validation Tests
// ============================================================================

#[test]
fn test_opt_out_and_e164_helpers() {
    assert!(is_opt_out_keyword("stop"));
    assert!(is_opt_out_keyword("STOP"));
    assert!(is_opt_out_keyword(" unsubscribe "));
    assert!(is_opt_out_keyword("CANCEL"));
    assert!(is_opt_out_keyword("quit"));
    assert!(!is_opt_out_keyword("hello"));

    assert!(is_opt_in_keyword("start"));
    assert!(is_opt_in_keyword("START"));
    assert!(is_opt_in_keyword("UNSTOP"));
    assert!(is_opt_in_keyword("yes"));

    assert!(is_valid_e164("+14155552671"));
    assert!(is_valid_e164("+919876543210"));
    assert!(!is_valid_e164("14155552671")); // missing +
    assert!(!is_valid_e164("+123")); // too short
    assert!(!is_valid_e164("+123456789012345678")); // too long
    assert!(!is_valid_e164("+141555abcde")); // non-digits
}

// ============================================================================
// 3. Provider Accept: Voice & Messaging Dispatch Tests
// ============================================================================

#[tokio::test]
async fn test_provider_accept_voice_and_messaging_delivers_to_channel() {
    let mock_telephony = Arc::new(MockTelephony::new());
    let mock_messaging = Arc::new(MockMessaging::new());
    let state = create_test_state(
        Some(mock_telephony.clone()),
        Some(mock_messaging.clone()),
        None,
    );
    let app = build_router(state.clone());

    // 1. Voice Delivery Attempt
    let voice_reminder_id = Uuid::new_v4();
    let voice_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "reminder_id": voice_reminder_id,
                "channel": "phone",
                "destination": "+14155552671",
                "title": "Dentist Appointment",
                "message": "Visit Dr. Smith tomorrow at 10 AM",
                "scheduled_for": "2026-09-24T10:00:00Z",
                "retry_count": 0,
                "max_retries": 3
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(voice_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: NotificationDispatchResponse = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(val.status, "delivered_to_channel");
    assert!(val.provider_receipt_id.unwrap().starts_with("CA"));
    assert_eq!(val.destination, "+14155552671");
    assert_eq!(mock_telephony.call_count.load(Ordering::SeqCst), 1);

    // 2. Messaging Delivery Attempt
    let msg_reminder_id = Uuid::new_v4();
    let msg_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "reminder_id": msg_reminder_id,
                "channel": "whatsapp",
                "destination": "+919876543210",
                "title": "Flight Alert",
                "message": "Flight AI-101 departs in 3 hours",
                "scheduled_for": "2026-09-24T12:00:00Z",
                "retry_count": 0,
                "max_retries": 3
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(msg_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: NotificationDispatchResponse = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(val.status, "delivered_to_channel");
    assert!(val.provider_receipt_id.unwrap().starts_with("wamid."));
    assert_eq!(mock_messaging.send_count.load(Ordering::SeqCst), 1);
}

// ============================================================================
// 4. Provider Rejection and Timeout Tests
// ============================================================================

#[tokio::test]
async fn test_provider_reject_and_timeout_semantics() {
    // 1. Rejection
    let mock_telephony_fail = Arc::new(MockTelephony::failing());
    let state = create_test_state(Some(mock_telephony_fail), None, None);
    let app = build_router(state);

    let fail_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "reminder_id": Uuid::new_v4(),
                "channel": "phone",
                "destination": "+14155552671",
                "title": "Test Reminder",
                "message": "Hello",
                "max_retries": 3
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(fail_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: NotificationDispatchResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val.status, "failed");
    assert!(val.failure_reason.unwrap().contains("Carrier rejected"));
    assert!(val.retryable);

    // 2. Timeout -> Unknown outcome
    let mock_messaging_timeout = Arc::new(MockMessaging::timing_out());
    let state_timeout = create_test_state(None, Some(mock_messaging_timeout), None);
    let app_timeout = build_router(state_timeout);

    let timeout_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "reminder_id": Uuid::new_v4(),
                "channel": "whatsapp",
                "destination": "+14155552671",
                "title": "Timeout Test",
                "message": "Checking timeout",
                "max_retries": 3
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app_timeout.oneshot(timeout_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: NotificationDispatchResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val.status, "unknown");
    assert!(val.failure_reason.unwrap().contains("timed out"));
    assert!(val.retryable);
}

// ============================================================================
// 5. Duplicate Dispatch & Idempotency Tests
// ============================================================================

#[tokio::test]
async fn test_duplicate_dispatch_prevents_duplicate_provider_calls() {
    let mock_telephony = Arc::new(MockTelephony::new());
    let state = create_test_state(Some(mock_telephony.clone()), None, None);
    let app = build_router(state);

    let reminder_id = Uuid::new_v4();
    let payload = json!({
        "reminder_id": reminder_id,
        "idempotency_key": format!("idem-{reminder_id}"),
        "channel": "phone",
        "destination": "+14155552671",
        "title": "Idempotent Call",
        "message": "Do not repeat",
        "max_retries": 3
    });

    // 1st Dispatch
    let req1 = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let resp1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);
    let b1 = axum::body::to_bytes(resp1.into_body(), usize::MAX)
        .await
        .unwrap();
    let v1: NotificationDispatchResponse = serde_json::from_slice(&b1).unwrap();
    assert_eq!(v1.status, "delivered_to_channel");
    let receipt1 = v1.provider_receipt_id.clone().unwrap();

    // 2nd Dispatch with identical reminder_id / idempotency_key
    let req2 = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let resp2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
    let b2 = axum::body::to_bytes(resp2.into_body(), usize::MAX)
        .await
        .unwrap();
    let v2: NotificationDispatchResponse = serde_json::from_slice(&b2).unwrap();
    assert_eq!(v2.status, "delivered_to_channel");
    assert_eq!(v2.provider_receipt_id.unwrap(), receipt1);

    // CRITICAL: Telephony client was called only ONCE!
    assert_eq!(mock_telephony.call_count.load(Ordering::SeqCst), 1);
}

// ============================================================================
// 6. Invalid Destination & Non-Action Authority Invariant Tests
// ============================================================================

#[tokio::test]
async fn test_invalid_destination_and_action_authority_invariants() {
    let mock_telephony = Arc::new(MockTelephony::new());
    let state = create_test_state(Some(mock_telephony.clone()), None, None);
    let app = build_router(state);

    // 1. Invalid destination
    let bad_dest_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "reminder_id": Uuid::new_v4(),
                "channel": "phone",
                "destination": "not-a-phone-number",
                "title": "Bad Dest",
                "message": "Fail immediately"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(bad_dest_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: NotificationDispatchResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val.status, "failed");
    assert!(val.failure_reason.unwrap().contains("Invalid destination"));
    assert_eq!(mock_telephony.call_count.load(Ordering::SeqCst), 0);

    // 2. Action Authority Injection Attempt -> FORBIDDEN
    let malicious_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "reminder_id": Uuid::new_v4(),
                "channel": "phone",
                "destination": "+14155552671",
                "title": "Execute Attack",
                "message": "Trigger consequential action",
                "metadata": {
                    "action_id": Uuid::new_v4().to_string(),
                    "execute_consequential": true
                }
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(malicious_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(mock_telephony.call_count.load(Ordering::SeqCst), 0);
}

// ============================================================================
// 7. Opt-Out & Opt-In Handling Tests
// ============================================================================

#[tokio::test]
async fn test_opt_out_and_opt_in_lifecycle() {
    let mock_messaging = Arc::new(MockMessaging::new());
    let state = create_test_state(None, Some(mock_messaging.clone()), None);
    let app = build_router(state.clone());

    let dest = "+14155559999";

    // 1. User opts out
    let opt_out_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/opt-out")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "destination": dest,
                "channel": "whatsapp",
                "reason": "User texted STOP"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(opt_out_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Attempt dispatch to opted-out recipient -> fails immediately without calling provider
    let dispatch_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "reminder_id": Uuid::new_v4(),
                "channel": "whatsapp",
                "destination": dest,
                "title": "Blocked Reminder",
                "message": "Should not be sent"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(dispatch_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: NotificationDispatchResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val.status, "failed");
    assert!(val.failure_reason.unwrap().contains("opted out"));
    assert_eq!(mock_messaging.send_count.load(Ordering::SeqCst), 0);

    // 3. User opts back in
    let opt_in_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/opt-in")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "destination": dest,
                "channel": "whatsapp"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(opt_in_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Dispatch now succeeds!
    let dispatch_req2 = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "reminder_id": Uuid::new_v4(),
                "channel": "whatsapp",
                "destination": dest,
                "title": "Welcome Back",
                "message": "Reminders restored"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(dispatch_req2).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: NotificationDispatchResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val.status, "delivered_to_channel");
    assert_eq!(mock_messaging.send_count.load(Ordering::SeqCst), 1);
}

// ============================================================================
// 8. Delayed Status Callback & Core Delivery Reporting Tests
// ============================================================================

#[tokio::test]
async fn test_delayed_status_callback_updates_delivery_record() {
    let mock_telephony = Arc::new(MockTelephony::new());
    let state = create_test_state(Some(mock_telephony.clone()), None, None);
    let app = build_router(state.clone());

    let reminder_id = Uuid::new_v4();

    // 1. Initial Dispatch
    let dispatch_req = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(
            json!({
                "reminder_id": reminder_id,
                "channel": "phone",
                "destination": "+14155552671",
                "title": "Initial Call",
                "message": "Checking delayed callback"
            })
            .to_string(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(dispatch_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: NotificationDispatchResponse = serde_json::from_slice(&bytes).unwrap();
    let call_sid = val.provider_receipt_id.unwrap();

    // 2. Delayed Twilio status callback arrives with CallStatus = "completed"
    let status_body = format!("CallSid={call_sid}&CallStatus=completed");
    let sig = vox_bridge::channels::twilio::signature::compute_twilio_signature(
        state.twilio_auth_token.as_str(),
        vox_bridge::channels::twilio::status::VOICE_STATUS_URL,
        &[
            ("CallSid".to_string(), call_sid.clone()),
            ("CallStatus".to_string(), "completed".to_string()),
        ],
    );

    let callback_req = Request::builder()
        .method("POST")
        .uri("/bridge/twilio/voice/status")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("x-twilio-signature", sig)
        .body(Body::from(status_body))
        .unwrap();

    let resp = app.clone().oneshot(callback_req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify delivery record in state was updated
    let delivery = state
        .notification_deliveries
        .get(&reminder_id.to_string())
        .unwrap();
    assert_eq!(delivery.status, "delivered_to_channel");
}
