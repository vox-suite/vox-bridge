use super::core_client::CoreAgentClient;
use crate::voice::{context::CallContext, provider::AgentProvider};
use axum::{Json, Router, http::HeaderMap, routing::post};
use futures_util::StreamExt;
use serde_json::{Value, json};

#[tokio::test]
async fn sends_authenticated_call_context_to_core() {
    let app = Router::new().route(
        "/v1/conversations/respond",
        post(|headers: HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(headers["authorization"], "Bearer service-token");
            assert_eq!(body["identity"], json!({"channel":"phone","external_id":"+919876543210"}));
            assert_eq!(body["external_conversation_id"], "CA123");
            assert_eq!(body["text"], "hello");
            Json(json!({"conversation_id":"f7f90d3d-5ded-4acf-850f-650bcb965fd1","text":"Hello Rahul"}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = CoreAgentClient::new(format!("http://{address}"), "service-token".into()).unwrap();
    let context = CallContext {
        channel: "phone".into(),
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
            assert_eq!(headers["authorization"], "Bearer service-token");
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
    let client = CoreAgentClient::new(format!("http://{address}"), "service-token".into()).unwrap();
    let context = CallContext {
        channel: "phone".into(),
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
    let client = CoreAgentClient::new(format!("http://{address}"), "service-token".into()).unwrap();
    let context = CallContext {
        channel: "phone".into(),
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
    let client = CoreAgentClient::new(format!("http://{address}"), "service-token".into())
        .unwrap()
        .with_tts_provider("elevenlabs");
    let context = CallContext {
        channel: "phone".into(),
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
