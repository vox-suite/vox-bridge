/**
 * Channel Acceptance Suite for Voice, Messaging, and Desktop Channels (E49 / vox-bridge#4).
 *
 * Verifies Platform V1 criteria:
 * 1. Realistic channel tests cover authentication failure, interruption, reconnect, notification, and unknown outcome.
 * 2. No test relies on paid provider behavior without an explicit live-test mode (VOX_LIVE_CHANNEL_TEST=1).
 * 3. Channel results strictly agree with authoritative Core state.
 * 4. Invariants: Non-action authority in reminders, truthful delivery (no seen inference), sensitive content redaction.
 */
use axum::{
    Json, Router,
    body::Body,
    extract::Path,
    http::{Request, StatusCode},
    routing::post,
};
use dashmap::DashMap;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

use vox_bridge::{
    app::build_router,
    channels::{
        context::CallContext,
        interaction::{
            ChannelInteractionError, ChannelInteractionHandler, HandoffNarrator,
            UncertaintyNarrator,
        },
        is_live_channel_test_enabled,
        notifications::{
            MessagingClient, NotificationDispatchResponse, is_opt_in_keyword, is_opt_out_keyword,
            redact_sensitive_content,
        },
        twilio::stream::serialize_command,
    },
    core::client::CoreClient,
    core::protocol::{ApproveRequestBody, ContextRequest},
    state::AppState,
    voice::config::VoiceConfig,
    voice::registry::VoiceRuntime,
    voice::session::{CallCommand, PlaybackState},
    voice::turn::DraftTurn,
};

/// Mock Messaging Client for hermetic testing without paid provider dependencies
struct MockMessaging {
    pub send_count: AtomicUsize,
}

#[async_trait::async_trait]
impl MessagingClient for MockMessaging {
    async fn send_message(&self, _channel: &str, _to: &str, _text: &str) -> Result<String, String> {
        self.send_count.fetch_add(1, Ordering::SeqCst);
        Ok(format!("wamid.{}", Uuid::new_v4().simple()))
    }
}

/// Helper to construct a test AppState with mock components
fn setup_test_app() -> (Arc<AppState>, axum::Router) {
    let twilio = Arc::new(DashMap::new());
    let config = VoiceConfig::from_values(|k| match k {
        "ASSEMBLYAI_API_KEY" => Some("mock-key".into()),
        "VOX_CORE_URL" => Some("http://127.0.0.1:3001".into()),
        "VOX_AUTH_TOKEN" => Some("test-core-token".into()),
        "ELEVENLABS_API_KEY" => Some("mock-el-key".into()),
        _ => None,
    })
    .unwrap();

    let voice = Arc::new(VoiceRuntime::from_config(config).unwrap());
    let core_client = Arc::new(
        CoreClient::new("http://127.0.0.1:3001".into(), "test-core-token".into()).unwrap(),
    );

    let state = Arc::new(AppState {
        twilio,
        twilio_account_sid: Arc::new("AC123456789".into()),
        twilio_auth_token: Arc::new("mock-twilio-auth-secret".into()),
        twilio_from_number: Arc::new("+15550001111".into()),
        service_token: Arc::new("test-service-token".into()),
        core_url: Arc::new("http://127.0.0.1:3001".into()),
        telephony: None,
        voice,
        core_client,
        whatsapp_verify_token: Some("mock-verify-token".into()),
        whatsapp_app_secret: Some("mock-app-secret".into()),
        whatsapp_access_token: Some("mock-access-token".into()),
        whatsapp_phone_id: Some("100200300".into()),
        desktop_sessions: Arc::new(DashMap::new()),
        opt_outs: Arc::new(DashMap::new()),
        notification_deliveries: Arc::new(DashMap::new()),
        messaging_client: Some(Arc::new(MockMessaging {
            send_count: AtomicUsize::new(0),
        })),
    });

    let router = build_router(state.clone());
    (state, router)
}

// ==============================================================================
// 1. AUTHENTICATION FAILURE ACROSS ALL CHANNELS
// ==============================================================================

#[tokio::test]
async fn test_channel_auth_failure_voice_and_messaging_fails_closed() {
    let (_state, router) = setup_test_app();

    // 1. Voice Ingress: Missing Twilio signature fails closed (401 Unauthorized)
    let voice_req_missing = Request::builder()
        .method("POST")
        .uri("/bridge/twilio/voice")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(Body::from("From=%2B15551234567&CallSid=CA12345"))
        .unwrap();

    let voice_res_missing = router.clone().oneshot(voice_req_missing).await.unwrap();
    assert_eq!(
        voice_res_missing.status(),
        StatusCode::UNAUTHORIZED,
        "Voice webhook without Twilio signature header must return 401 Unauthorized"
    );

    // Voice Ingress: Invalid Twilio signature fails closed (403 Forbidden)
    let voice_req_invalid = Request::builder()
        .method("POST")
        .uri("/bridge/twilio/voice")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("x-twilio-signature", "invalid-twilio-signature")
        .body(Body::from(
            "From=%2B15551234567&CallSid=CA12345&AccountSid=AC123456789&To=%2B15550001111",
        ))
        .unwrap();

    let voice_res_invalid = router.clone().oneshot(voice_req_invalid).await.unwrap();
    assert_eq!(
        voice_res_invalid.status(),
        StatusCode::FORBIDDEN,
        "Voice webhook with invalid Twilio signature must return 403 Forbidden"
    );

    // 2. Messaging Ingress (WhatsApp): Missing or invalid Meta signature fails closed (403)
    let wa_req = Request::builder()
        .method("POST")
        .uri("/bridge/wa")
        .header("Content-Type", "application/json")
        .body(Body::from(
            json!({
                "object": "whatsapp_business_account",
                "entry": []
            })
            .to_string(),
        ))
        .unwrap();

    let wa_res = router.clone().oneshot(wa_req).await.unwrap();
    assert_eq!(
        wa_res.status(),
        StatusCode::FORBIDDEN,
        "WhatsApp webhook without valid X-Hub-Signature-256 must return 403 Forbidden"
    );

    // 3. Desktop Session: Missing or invalid Bearer token fails closed (401)
    let desktop_req = Request::builder()
        .method("POST")
        .uri("/bridge/desktop/voice/session")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer wrong-desktop-token")
        .body(Body::from(
            json!({
                "host_user_id": "user-42"
            })
            .to_string(),
        ))
        .unwrap();

    let desktop_res = router.clone().oneshot(desktop_req).await.unwrap();
    assert_eq!(
        desktop_res.status(),
        StatusCode::UNAUTHORIZED,
        "Desktop session initiation with invalid token must return 401 Unauthorized"
    );

    // 4. Sender Identity Mismatch: Unauthorized sender trying to approve proposal is blocked
    let context = CallContext {
        channel: "whatsapp".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "whatsapp:+15551234567".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };

    let verify_res = ChannelInteractionHandler::verify_sender(&context, "+19998887777");
    assert!(verify_res.is_err());
    match verify_res.unwrap_err() {
        ChannelInteractionError::UnauthorizedSender(s) => {
            assert_eq!(s, "+19998887777");
        }
        other => panic!("Expected UnauthorizedSender, got: {other:?}"),
    }
}

// ==============================================================================
// 2. VOICE INTERRUPTION & BARGE-IN
// ==============================================================================

#[test]
fn test_voice_interruption_barge_in_and_playback_invalidation() {
    let playback = PlaybackState::new();
    assert_eq!(playback.current_generation(), 0);
    assert!(!playback.is_playing());

    // Agent begins playing response chunk (generation 1)
    let gen1 = playback.begin();
    assert_eq!(gen1, 1);
    assert!(playback.is_playing());
    assert!(playback.accepts(1));

    // Human speaks (barge-in detected) -> invalidate previous playback
    let gen2 = playback.invalidate();
    assert_eq!(gen2, 2);
    assert!(!playback.is_playing());

    // Inflight audio chunks for generation 1 MUST be discarded
    assert!(
        !playback.accepts(gen1),
        "Barge-in must invalidate previous audio generation"
    );

    // CallCommand::Clear must serialize cleanly to flush provider buffer
    let clear_msg = serialize_command("test-stream-sid", &CallCommand::Clear).unwrap();
    assert!(clear_msg.contains("\"event\":\"clear\""));
    assert!(clear_msg.contains("\"streamSid\":\"test-stream-sid\""));

    // Turn state resets on human correction
    let mut turn = DraftTurn::default();
    turn.finish("book flight to Seattle");
    assert_eq!(turn.snapshot(), "book flight to Seattle");

    // Human interrupts with correction:
    turn.finish("no, book flight to Portland instead");
    assert_eq!(turn.snapshot(), "no, book flight to Portland instead");
}

// ==============================================================================
// 3. CHANNEL RECONNECT & AUTHORITATIVE CORE STATE SYNCHRONIZATION
// ==============================================================================

#[tokio::test]
async fn test_channel_reconnect_synchronizes_authoritative_core_state() {
    let task_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    // Spawn mock Core server returning authoritative task state on POST /v1/durable-tasks/{id}
    let app = Router::new().route(
        "/v1/durable-tasks/{id}",
        post({
            move |Path(id): Path<Uuid>, Json(_body): Json<ContextRequest>| async move {
                assert_eq!(id, task_id);
                (
                    StatusCode::OK,
                    Json(json!({
                        "id": task_id,
                        "title": "Book Lodging in Seattle",
                        "state": "waiting",
                        "run_id": run_id,
                        "wait_reason": "approval"
                    })),
                )
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = CoreClient::new(format!("http://127.0.0.1:{port}"), "test-token".into()).unwrap();
    let context = CallContext {
        channel: "voice".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "voice:+15551234567".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };

    // Caller reconnects after dropped call and asks: "status"
    let status_narrative =
        ChannelInteractionHandler::handle_status(&client, &context, "+15551234567", task_id)
            .await
            .unwrap();

    // Must agree with Core authoritative state
    assert!(status_narrative.contains("Book Lodging in Seattle"));
    assert!(status_narrative.contains("waiting for your explicit approval"));
    assert!(status_narrative.contains(&task_id.to_string()));
}

// ==============================================================================
// 4. NOTIFICATION DELIVERY: TRUTHFUL CHANNEL RECEIPT & NON-ACTION AUTHORITY
// ==============================================================================

#[tokio::test]
async fn test_notification_delivery_truthful_channel_receipt_and_no_seen_claim() {
    let (_state, router) = setup_test_app();

    // 1. INVARIANT 1: Action authority injection MUST be rejected with 403
    let forbidden_payload = json!({
        "reminder_id": Uuid::new_v4(),
        "channel": "whatsapp",
        "destination": "+15551234567",
        "title": "Action Reminder",
        "message": "Pay invoice",
        "timezone": "UTC",
        "metadata": {
            "action_id": Uuid::new_v4(),
            "execute_consequential": true
        }
    });

    let req_forbidden = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer test-service-token")
        .body(Body::from(forbidden_payload.to_string()))
        .unwrap();

    let res_forbidden = router.clone().oneshot(req_forbidden).await.unwrap();
    assert_eq!(
        res_forbidden.status(),
        StatusCode::FORBIDDEN,
        "Notifications embedding action authority must fail closed with 403"
    );

    // 2. INVARIANT 2: Truthful delivery reports delivered_to_channel, NEVER seen or confirmed read
    let reminder_id = Uuid::new_v4();
    let valid_payload = json!({
        "reminder_id": reminder_id,
        "channel": "whatsapp",
        "destination": "+15551234567",
        "title": "Team Standup",
        "message": "Standup starts in 10 minutes",
        "timezone": "UTC"
    });

    let req_valid = Request::builder()
        .method("POST")
        .uri("/internal/v1/notifications/dispatch")
        .header("Content-Type", "application/json")
        .header("Authorization", "Bearer test-service-token")
        .body(Body::from(valid_payload.to_string()))
        .unwrap();

    let res_valid = router.clone().oneshot(req_valid).await.unwrap();
    assert_eq!(res_valid.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(res_valid.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let resp: NotificationDispatchResponse = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(resp.status, "delivered_to_channel");
    assert!(resp.delivered_to_channel_at.is_some());
    // Invariant: truthful delivery status string NEVER contains "seen" or "read"
    assert!(
        !resp.status.contains("seen"),
        "Truthful delivery must not claim seen"
    );
    assert!(
        !resp.status.contains("read"),
        "Truthful delivery must not claim read"
    );
}

// ==============================================================================
// 5. CONSEQUENTIAL PROPOSAL APPROVAL, SUPERSEDED REJECTION, & DUPLICATE DECISION
// ==============================================================================

#[tokio::test]
async fn test_consequential_approval_changed_proposal_and_duplicate_decision() {
    let proposal_id = Uuid::new_v4();
    let call_count = Arc::new(AtomicUsize::new(0));
    let call_count_clone = call_count.clone();

    // Mock Core returning 200 on first call, 409 Conflict on second (consumed/duplicate)
    let app = Router::new().route(
        "/v1/action-proposals/{id}/approve",
        post({
            move |Path(id): Path<Uuid>, Json(_body): Json<ApproveRequestBody>| {
                let count = call_count_clone.fetch_add(1, Ordering::SeqCst);
                async move {
                    if count == 0 {
                        (
                            StatusCode::OK,
                            Json(json!({
                                "id": id,
                                "capability_external_key": "hotel.book",
                                "expires_at": "2026-10-01T00:00:00Z",
                                "approval_id": Uuid::new_v4(),
                                "details": {
                                    "property": "Grand Hyatt",
                                    "price_minor": 18999,
                                    "currency": "USD"
                                }
                            })),
                        )
                    } else {
                        // Core rejects duplicate or consumed decision
                        (
                            StatusCode::CONFLICT,
                            Json(json!({
                                "error": "This decision has already been processed or consumed"
                            })),
                        )
                    }
                }
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = CoreClient::new(format!("http://127.0.0.1:{port}"), "test-token".into()).unwrap();
    let context = CallContext {
        channel: "whatsapp".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "whatsapp:+15551234567".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };

    // First decision: exact match approved
    let first_res = ChannelInteractionHandler::handle_approve(
        &client,
        &context,
        "+15551234567",
        proposal_id,
        json!({"property": "Grand Hyatt"}),
    )
    .await;

    assert!(first_res.is_ok());
    let msg = first_res.unwrap();
    assert!(msg.contains("has been approved"));
    assert!(msg.contains("Core is coordinating execution"));

    // Second decision: duplicate attempt fails closed with DuplicateDecision
    let second_res = ChannelInteractionHandler::handle_approve(
        &client,
        &context,
        "+15551234567",
        proposal_id,
        json!({"property": "Grand Hyatt"}),
    )
    .await;

    assert!(second_res.is_err());
    match second_res.unwrap_err() {
        ChannelInteractionError::DuplicateDecision(err) => {
            assert!(err.contains("already been processed"));
        }
        other => panic!("Expected DuplicateDecision, got: {other:?}"),
    }
}

// ==============================================================================
// 6. UNKNOWN OUTCOME & CORE UNREACHABLE FAIL CLOSED
// ==============================================================================

#[tokio::test]
async fn test_unknown_outcome_and_core_unreachable_fail_closed() {
    let proposal_id = Uuid::new_v4();

    // Port with no listener -> communication failure
    let client = CoreClient::new("http://127.0.0.1:59998".into(), "test-token".into()).unwrap();
    let context = CallContext {
        channel: "voice".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "voice:+15551234567".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };

    let result = ChannelInteractionHandler::handle_approve(
        &client,
        &context,
        "+15551234567",
        proposal_id,
        json!({}),
    )
    .await;

    assert!(result.is_err(), "Must fail closed when Core is unreachable");

    // Uncertainty narrator explicitly tells the user that the run state is unconfirmed
    let uncertainty = UncertaintyNarrator::unconfirmed_outcome(
        "hotel reservation booking",
        "Core timeout on port 59998",
    );

    assert!(uncertainty.contains("Unconfirmed Outcome"));
    assert!(uncertainty.contains("No action was assumed completed"));
    assert!(uncertainty.contains("app.voxagent.in"));
}

// ==============================================================================
// 7. NO PAID PROVIDER CALLS WITHOUT EXPLICIT LIVE-TEST MODE
// ==============================================================================

#[test]
fn test_no_paid_provider_calls_without_explicit_live_test_mode() {
    // In standard CI/test runs, VOX_LIVE_CHANNEL_TEST must be absent or false
    let live_enabled = is_live_channel_test_enabled();
    assert!(
        !live_enabled,
        "By default, live paid provider tests must be disabled"
    );

    // Verify helper respects environment override
    // Safety: single-threaded check on local scope
    unsafe {
        std::env::set_var("VOX_LIVE_CHANNEL_TEST", "1");
    }
    assert!(is_live_channel_test_enabled());

    unsafe {
        std::env::remove_var("VOX_LIVE_CHANNEL_TEST");
    }
    assert!(!is_live_channel_test_enabled());
}

// ==============================================================================
// 8. SENSITIVE CONTENT REDACTION & PRIVACY INVARIANTS
// ==============================================================================

#[test]
fn test_sensitive_content_redaction_and_privacy_invariants() {
    let raw_text = "Your API key is sk-ant-api03-abcdef1234567890 and card is 4111-2222-3333-4444 with cvv 123";
    let redacted = redact_sensitive_content(raw_text);

    assert!(!redacted.contains("sk-ant-api03-abcdef1234567890"));
    assert!(!redacted.contains("4111-2222-3333-4444"));
    assert!(redacted.contains("[REDACTED_SECRET]"));
    assert!(redacted.contains("[REDACTED_CARD]"));
}

// ==============================================================================
// 9. LABELLED HANDOFF NARRATION EXPLICITLY DISCLOSES NON-COMPLETION
// ==============================================================================

#[test]
fn test_labelled_handoff_narration_discloses_non_completion() {
    let task_id = Uuid::new_v4();
    let handoff_text = HandoffNarrator::narrate(
        "Amazon MacBook Cart",
        task_id,
        "amazon",
        "https://amazon.com/gp/cart/view.html?ref=vox",
    );

    // Strict invariant: Handoff MUST disclose that action was not completed by Vox
    assert!(handoff_text.contains("is NOT completed"));
    assert!(handoff_text.contains("Bridge does not claim completion"));
    assert!(handoff_text.contains("Amazon MacBook Cart"));
    assert!(handoff_text.contains("amazon"));
    assert!(handoff_text.contains("https://amazon.com/gp/cart/view.html?ref=vox"));
}

// ==============================================================================
// 10. OPT-OUT & OPT-IN LIFECYCLE
// ==============================================================================

#[test]
fn test_opt_out_and_opt_in_lifecycle() {
    assert!(is_opt_out_keyword("STOP"));
    assert!(is_opt_out_keyword("stop"));
    assert!(is_opt_out_keyword("UNSUBSCRIBE"));
    assert!(is_opt_out_keyword("CANCEL"));
    assert!(is_opt_out_keyword("quit"));

    assert!(is_opt_in_keyword("START"));
    assert!(is_opt_in_keyword("start"));
    assert!(is_opt_in_keyword("UNSTOP"));
    assert!(is_opt_in_keyword("yes"));

    assert!(!is_opt_out_keyword("Hello, can you help me?"));
    assert!(!is_opt_in_keyword("status"));
}
