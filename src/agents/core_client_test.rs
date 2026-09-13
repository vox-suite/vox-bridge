use super::core_client::CoreAgentClient;
use crate::voice::{context::CallContext, provider::AgentProvider};
use axum::{Json, Router, http::HeaderMap, routing::post};
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
    };

    let response = client.respond(&context, "hello").await.unwrap();

    assert_eq!(response, "Hello Rahul");
}
