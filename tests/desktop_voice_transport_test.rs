use vox_bridge::channels::desktop::protocol::{Control, parse_control};
#[test]
fn rejects_untrusted_or_oversized_controls() {
    assert!(parse_control(r#"{"type":"start","ticket":""}"#).is_err());
    assert!(parse_control(r#"{"type":"start","ticket":"valid","user_id":"forged"}"#).is_err());
    assert!(parse_control(&"x".repeat(8193)).is_err());
}
#[test]
fn parses_mark_without_changing_identity() {
    assert!(
        matches!(parse_control(r#"{"type":"playback_finished","name":"response-1"}"#).unwrap(), Control::PlaybackFinished { name } if name == "response-1")
    );
}

#[tokio::test]
async fn redeemed_session_scopes_stream_and_completion_without_forwarding_identity() {
    use axum::{Json, Router, extract::Path, http::HeaderMap, routing::post};
    use serde_json::{Value, json};
    use vox_bridge::{
        channels::context::CallContext,
        core::{ConversationClient, desktop::DesktopConversationClient},
    };
    let session = uuid::Uuid::new_v4().to_string();
    let bound = session.clone();
    let app = Router::new()
        .route(
            "/internal/v1/desktop-voice/redeem",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let session = bound.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer test-service-only");
                    assert_eq!(body, json!({"ticket":"test-ticket"}));
                    Json(json!({"session_id":session,"user_id":"trusted-user","device_id":null}))
                }
            }),
        )
        .route(
            "/internal/v1/desktop-voice/{id}/stream",
            post(
                |Path(id): Path<String>, headers: HeaderMap, Json(body): Json<Value>| async move {
                    assert!(uuid::Uuid::parse_str(&id).is_ok());
                    assert_eq!(headers["authorization"], "Bearer test-service-only");
                    assert!(body.get("user_id").is_none());
                    assert!(body.get("external_identity").is_none());
                    assert_eq!(body["text"], "Open timeline");
                    "data: {\"delta\":\"Opened timeline\"}\n\ndata: [DONE]\n\n"
                },
            ),
        )
        .route(
            "/internal/v1/desktop-voice/{id}/complete",
            post(|Path(id): Path<String>| async move {
                assert!(uuid::Uuid::parse_str(&id).is_ok());
                Json(json!({"ok":true}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (client, binding) =
        DesktopConversationClient::redeem(&base, "test-service-only", "test-ticket")
            .await
            .unwrap();
    assert_eq!(binding.session_id, session);
    let context = CallContext {
        channel: "forged".into(),
        external_identity: "wrong-user".into(),
        external_conversation_id: "wrong-session".into(),
        initiation_context: None,
        turn_id: Some("one".into()),
        revision: Some(1),
        tts_provider: None,
        filler: None,
    };
    assert_eq!(
        client.respond(&context, "Open timeline").await.unwrap(),
        "Opened timeline"
    );
    client.complete(&context).await.unwrap();
    server.abort();
}
