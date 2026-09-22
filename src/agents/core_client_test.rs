use super::core_client::CoreAgentClient;
use crate::voice::{context::CallContext, provider::AgentProvider};
use axum::{Json, Router, http::HeaderMap, routing::post};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn sends_authenticated_call_context_to_core() {
    let app = Router::new().route(
        "/v1/conversations/respond",
        post(|headers: HeaderMap, Json(body): Json<Value>| async move {
            assert!(headers.get("authorization").is_none());
            assert_eq!(body["host_context"]["host_user_id"], "twilio:+919876543210");
            assert_eq!(body["channel"], "twilio");
            assert!(body.get("identity").is_none());
            assert_eq!(body["external_conversation_id"], "CA123");
            assert_eq!(body["text"], "hello");
            Json(json!({"conversation_id":"f7f90d3d-5ded-4acf-850f-650bcb965fd1","text":"Hello Rahul"}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = client(format!("http://{address}"));
    let context = CallContext {
        channel: "twilio".into(),
        external_identity: "+919876543210".into(),
        external_conversation_id: "CA123".into(),
        initiation_context: None,
        voice_signature: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
    };

    let response = client.respond(&context, "hello").await.unwrap();

    assert_eq!(response, "Hello Rahul");
}

#[tokio::test]
async fn streams_sse_tokens_from_core_stream_endpoint() {
    let app = Router::new().route(
        "/v1/conversations/respond/stream",
        post(|headers: HeaderMap, Json(body): Json<Value>| async move {
            assert!(headers.get("authorization").is_none());
            assert_eq!(headers["accept"], "text/event-stream");
            assert_eq!(body["text"], "hello");
            let sse_body =
                "data: {\"delta\":\"Hello \"}\n\ndata: {\"delta\":\"Rahul!\"}\n\ndata: [DONE]\n\n";
            axum::response::Response::builder()
                .header("content-type", "text/event-stream")
                .body(axum::body::Body::from(sse_body))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = client(format!("http://{address}"));
    let context = CallContext {
        channel: "twilio".into(),
        external_identity: "+919876543210".into(),
        external_conversation_id: "CA123".into(),
        initiation_context: None,
        voice_signature: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
    };

    let mut stream = client.respond_stream(&context, "hello").await.unwrap();
    let mut chunks = Vec::new();
    while let Some(chunk) = stream.next().await {
        chunks.push(chunk.unwrap());
    }

    assert_eq!(chunks, vec!["Hello ".to_string(), "Rahul!".to_string()]);
}

#[tokio::test]
async fn stream_falls_back_to_unary_when_stream_endpoint_returns_404() {
    let app = Router::new().route(
        "/v1/conversations/respond",
        post(|_headers: HeaderMap, _body: Json<Value>| async move {
            Json(json!({"conversation_id":"f7f90d3d-5ded-4acf-850f-650bcb965fd1","text":"Fallback Response"}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = client(format!("http://{address}"));
    let context = CallContext {
        channel: "twilio".into(),
        external_identity: "+919876543210".into(),
        external_conversation_id: "CA123".into(),
        initiation_context: None,
        voice_signature: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
    };

    let mut stream = client.respond_stream(&context, "hello").await.unwrap();
    let mut chunks = Vec::new();
    while let Some(chunk) = stream.next().await {
        chunks.push(chunk.unwrap());
    }

    assert_eq!(chunks, vec!["Fallback Response".to_string()]);
}

#[tokio::test]
async fn propagates_tts_provider_to_core() {
    let app = Router::new().route(
        "/v1/conversations/respond",
        post(|_headers: HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(body["tts_provider"], "elevenlabs");
            Json(json!({"conversation_id":"f7f90d3d-5ded-4acf-850f-650bcb965fd1","text":"[thoughtful] Hello Rahul"}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = client(format!("http://{address}")).with_tts_provider("elevenlabs");
    let context = CallContext {
        channel: "twilio".into(),
        external_identity: "+919876543210".into(),
        external_conversation_id: "CA123".into(),
        initiation_context: None,
        voice_signature: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
    };

    let response = client.respond(&context, "hello").await.unwrap();
    assert_eq!(response, "[thoughtful] Hello Rahul");
}

#[tokio::test]
async fn signs_each_unary_speculation_and_completion_request_with_a_fresh_nonce() {
    let nonces = Arc::new(Mutex::new(Vec::<String>::new()));
    let handler = |nonces: Arc<Mutex<Vec<String>>>| {
        move |headers: HeaderMap, Json(body): Json<Value>| {
            let nonces = nonces.clone();
            async move {
                let nonce = headers
                    .get("x-vox-host-nonce")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                nonces.lock().unwrap().push(nonce);
                assert!(headers.get("x-vox-host-signature").is_some());
                assert!(headers.get("x-vox-host-secret").is_some());
                assert_eq!(body["host_context"]["host_user_id"], "twilio:+919876543210");
                assert_eq!(body["channel"], "twilio");
                assert!(body.get("identity").is_none());
                Json(json!({"conversation_id":"f7f90d3d-5ded-4acf-850f-650bcb965fd1","text":"ok"}))
            }
        }
    };
    let app = Router::new()
        .route("/v1/conversations/respond", post(handler(nonces.clone())))
        .route("/v1/conversations/speculate", post(handler(nonces.clone())))
        .route("/v1/conversations/complete", post(handler(nonces.clone())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = client(format!("http://{address}"));
    let context = CallContext {
        channel: "twilio".into(),
        external_identity: "+919876543210".into(),
        external_conversation_id: "CA123".into(),
        initiation_context: None,
        voice_signature: None,
        turn_id: Some("turn-1".into()),
        revision: Some(1),
        tts_provider: None,
    };
    client.respond(&context, "hello").await.unwrap();
    client.speculate(&context, "hello").await.unwrap();
    client.complete(&context).await.unwrap();
    let observed = nonces.lock().unwrap();
    assert_eq!(observed.len(), 3);
    let unique: std::collections::HashSet<_> = observed.iter().collect();
    assert_eq!(unique.len(), 3);
}

#[tokio::test]
async fn same_phone_on_twilio_and_whatsapp_remains_two_host_users() {
    let subjects = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = subjects.clone();
    let app = Router::new().route(
        "/v1/conversations/respond",
        post(move |Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                captured.lock().unwrap().push(
                    body["host_context"]["host_user_id"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
                Json(json!({"conversation_id":"f7f90d3d-5ded-4acf-850f-650bcb965fd1","text":"ok"}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = client(format!("http://{address}"));
    let context = |channel: &str| CallContext {
        channel: channel.into(),
        external_identity: "+919876543210".into(),
        external_conversation_id: format!("{channel}:conversation"),
        initiation_context: None,
        voice_signature: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
    };
    client.respond(&context("twilio"), "hello").await.unwrap();
    client.respond(&context("whatsapp"), "hello").await.unwrap();
    assert_eq!(
        *subjects.lock().unwrap(),
        ["twilio:+919876543210", "whatsapp:+919876543210"]
    );
}

#[tokio::test]
async fn malformed_sender_fails_before_any_core_request() {
    let client = client("http://127.0.0.1:1".into());
    let context = CallContext {
        channel: "twilio".into(),
        external_identity: "unknown-caller".into(),
        external_conversation_id: "CA-invalid".into(),
        initiation_context: None,
        voice_signature: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
    };
    let error = client.respond(&context, "hello").await.unwrap_err();
    assert!(error.to_string().contains("configuration"));
}

fn client(base_url: String) -> CoreAgentClient {
    CoreAgentClient::new(
        base_url,
        "f7f90d3d-5ded-4acf-850f-650bcb965fd1".into(),
        "vox-host:development:bridge".into(),
        "host-secret".into(),
    )
    .unwrap()
}
