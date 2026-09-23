/**
* this file code contains tests for typesafe jev client and filler evaluation
*/
use axum::{Json, Router, extract::Json as ExtractJson, routing::post};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use vox_bridge::providers::jev::JevClient;
use vox_bridge::voice::filler::filler_for_choice;

#[tokio::test]
async fn evaluates_jev_choice_primitive_successfully() {
    let app = Router::new().route(
        "/v1/systemone",
        post(|| async {
            Json(json!({
                "model": "jev-1.13.0",
                "answers": {
                    "filler": {
                        "type": "choice",
                        "choice": "check_order",
                        "confidence": 0.98,
                        "probabilities": {
                            "check_order": 0.98,
                            "looking_into_that": 0.02
                        }
                    }
                }
            }))
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let http = reqwest::Client::new();
    let client = JevClient::new(http, "test-api-key".into())
        .with_endpoint(format!("http://127.0.0.1:{port}/v1/systemone"));

    let choice = client
        .choose_filler("Where is my package? Has it arrived yet?")
        .await
        .unwrap();

    assert_eq!(choice, "check_order");
    assert_eq!(filler_for_choice(&choice), "Let me check on your order.");
}

#[tokio::test]
async fn evaluates_thought_completeness_with_noul() {
    let app = Router::new().route(
        "/v1/systemone",
        post(|ExtractJson(payload): ExtractJson<Value>| async move {
            let state = payload["state"].as_str().unwrap_or("");
            let noul = if state.ends_with("...") || state.ends_with("and") {
                0.15
            } else {
                0.92
            };
            Json(json!({
                "model": "jev-1.13.0",
                "answers": {
                    "is_complete": {
                        "type": "noul",
                        "noul": noul
                    }
                }
            }))
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let http = reqwest::Client::new();
    let client = JevClient::new(http, "test-api-key".into())
        .with_endpoint(format!("http://127.0.0.1:{port}/v1/systemone"));

    let prob_complete = client
        .is_complete_thought("What time do you open?")
        .await
        .unwrap();
    assert!(prob_complete > 0.85);

    let prob_incomplete = client
        .is_complete_thought("I was wondering if and")
        .await
        .unwrap();
    assert!(prob_incomplete < 0.35);
}

#[tokio::test]
async fn handles_jev_failure_gracefully() {
    let http = reqwest::Client::new();
    let client = JevClient::new(http, "test-api-key".into())
        .with_endpoint("http://127.0.0.1:9999/v1/systemone".into());

    let result = client.choose_filler("Any question").await;
    assert!(result.is_err());
}
