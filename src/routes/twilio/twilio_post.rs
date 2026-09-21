use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::sync::Arc;

use crate::AppState;

pub(crate) const VOICE_WEBHOOK_URL: &str = "https://api.voxagent.in/bridge/twilio/voice";
pub(crate) const VOICE_STREAM_URL: &str = "wss://api.voxagent.in/bridge/twilio/voice/stream";

type HmacSha1 = Hmac<Sha1>;

#[derive(Clone, Deserialize, Serialize)]
pub struct TwilioState {
    pub call_sid: String,
    pub account_sid: String,
    pub from: String,
    pub to: String,
    pub call_status: Option<String>,
    pub opening_instruction: Option<String>,
    pub action_id: Option<String>,
    pub external_conversation_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct TwilioPostParams {
    pub call_sid: String,
    pub account_sid: String,
    pub from: String,
    pub to: String,
    pub call_status: Option<String>,
}

pub async fn initialize_voice_socket(
    State(app_state): State<Arc<AppState>>,
    header: HeaderMap,
    body: String,
) -> Response {
    let Some(signature) = header
        .get("x-twilio-signature")
        .and_then(|value| value.to_str().ok())
    else {
        tracing::warn!("rejected Twilio webhook without a valid signature header");
        return (StatusCode::UNAUTHORIZED, "Missing Twilio signature").into_response();
    };

    let all_parameters: Vec<(String, String)> = match serde_urlencoded::from_str(&body) {
        Ok(parameters) => parameters,
        Err(error) => {
            tracing::warn!(%error, "invalid Twilio webhook form body");
            return (StatusCode::BAD_REQUEST, "Invalid form body").into_response();
        }
    };

    let params: TwilioPostParams = match serde_urlencoded::from_str(&body) {
        Ok(params) => params,
        Err(error) => {
            tracing::warn!(%error, "Twilio webhook is missing required call parameters");
            return (StatusCode::BAD_REQUEST, "Invalid form body").into_response();
        }
    };

    if !validate_twilio_signature(
        app_state.twilio_auth_token.as_str(),
        VOICE_WEBHOOK_URL,
        &all_parameters,
        signature,
    ) {
        tracing::warn!("rejected Twilio webhook with an invalid signature");
        return (StatusCode::FORBIDDEN, "Invalid Twilio signature").into_response();
    }

    let call_sid = params.call_sid.clone();
    let external_conversation_id = params.call_sid.clone();
    app_state.twilio.insert(
        params.call_sid.clone(),
        TwilioState {
            call_sid: params.call_sid,
            account_sid: params.account_sid,
            from: params.from,
            to: params.to,
            call_status: params.call_status,
            opening_instruction: Some("The call just connected. Greet the user.".into()),
            action_id: None,
            external_conversation_id,
        },
    );

    tracing::info!(%call_sid, "accepted incoming Twilio call");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml")],
        voice_twiml(),
    )
        .into_response()
}

fn voice_twiml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Connect>
    <Stream url="{VOICE_STREAM_URL}" />
  </Connect>
</Response>"#
    )
}

#[cfg(test)]
fn expected_twilio_signature(
    auth_token: &str,
    public_url: &str,
    form_parameters: &[(String, String)],
) -> String {
    let mut parameters = form_parameters.to_vec();
    parameters.sort_by(|left, right| left.0.cmp(&right.0));

    let mut signed_data = public_url.to_owned();
    for (name, value) in parameters {
        signed_data.push_str(&name);
        signed_data.push_str(&value);
    }

    let mut mac =
        HmacSha1::new_from_slice(auth_token.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(signed_data.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

pub(crate) fn validate_twilio_signature(
    auth_token: &str,
    public_url: &str,
    form_parameters: &[(String, String)],
    received_signature: &str,
) -> bool {
    let Ok(received_signature) = STANDARD.decode(received_signature) else {
        return false;
    };

    let mut parameters = form_parameters.to_vec();
    parameters.sort_by(|left, right| left.0.cmp(&right.0));

    let mut signed_data = public_url.to_owned();
    for (name, value) in parameters {
        signed_data.push_str(&name);
        signed_data.push_str(&value);
    }

    let Ok(mut mac) = HmacSha1::new_from_slice(auth_token.as_bytes()) else {
        return false;
    };
    mac.update(signed_data.as_bytes());
    mac.verify_slice(&received_signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::StatusCode};
    use dashmap::DashMap;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    fn test_state(auth_token: &str) -> Arc<AppState> {
        let (tx, _rx) = broadcast::channel(1);
        let values = HashMap::from([
            ("ASSEMBLYAI_API_KEY".to_owned(), "assembly-key".to_owned()),
            ("VOX_CORE_URL".to_owned(), "http://core-api:3001".to_owned()),
            (
                "VOX_CORE_SERVICE_TOKEN".to_owned(),
                "service-token".to_owned(),
            ),
            ("ELEVENLABS_API_KEY".to_owned(), "eleven-key".to_owned()),
        ]);
        let voice_config =
            crate::voice::config::VoiceConfig::from_values(|key| values.get(key).cloned()).unwrap();

        Arc::new(AppState {
            tx,
            twilio: Arc::new(DashMap::new()),
            twilio_account_sid: Arc::new("AC123".to_owned()),
            twilio_auth_token: Arc::new(auth_token.to_owned()),
            twilio_from_number: Arc::new("+14155550100".to_owned()),
            service_token: Arc::new("service-token".to_owned()),
            core_url: Arc::new("http://core-api:3001".to_owned()),
            telephony: None,
            voice: Arc::new(
                crate::voice::registry::VoiceRuntime::from_config(voice_config).unwrap(),
            ),
        })
    }

    #[test]
    fn validates_twilio_official_signature_example() {
        let parameters = vec![
            ("Digits".to_owned(), "1234".to_owned()),
            ("To".to_owned(), "+18005551212".to_owned()),
            ("From".to_owned(), "+14158675310".to_owned()),
            ("Caller".to_owned(), "+14158675310".to_owned()),
            ("CallSid".to_owned(), "CA1234567890ABCDE".to_owned()),
        ];

        assert!(validate_twilio_signature(
            "12345",
            "https://example.com/myapp.php?foo=1&bar=2",
            &parameters,
            "L/OH5YylLD5NRKLltdqwSvS0BnU=",
        ));
    }

    #[tokio::test]
    async fn rejects_a_request_without_a_signature() {
        let state = test_state("test-token");

        let response = initialize_voice_socket(
            State(state.clone()),
            HeaderMap::new(),
            "CallSid=CA123&AccountSid=AC123&From=%2B14155550100&To=%2B14155550101".to_owned(),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(state.twilio.is_empty());
    }

    #[tokio::test]
    async fn rejects_an_invalid_signature_without_creating_a_session() {
        let state = test_state("test-token");
        let mut headers = HeaderMap::new();
        headers.insert("x-twilio-signature", "aW52YWxpZA==".parse().unwrap());

        let response = initialize_voice_socket(
            State(state.clone()),
            headers,
            "CallSid=CA123&AccountSid=AC123&From=%2B14155550100&To=%2B14155550101".to_owned(),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(state.twilio.is_empty());
    }

    #[tokio::test]
    async fn rejects_a_malformed_form_body() {
        let state = test_state("test-token");
        let mut headers = HeaderMap::new();
        headers.insert("x-twilio-signature", "aW52YWxpZA==".parse().unwrap());

        let response =
            initialize_voice_socket(State(state.clone()), headers, "CallSid=%ZZ".to_owned())
                .await
                .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(state.twilio.is_empty());
    }

    #[tokio::test]
    async fn verified_request_creates_session_and_returns_stream_twiml() {
        let state = test_state("test-token");
        let body = "CallSid=CA123&AccountSid=AC123&From=%2B14155550100&To=%2B14155550101";
        let parameters: Vec<(String, String)> = serde_urlencoded::from_str(body).unwrap();
        let signature = expected_twilio_signature("test-token", VOICE_WEBHOOK_URL, &parameters);
        let mut headers = HeaderMap::new();
        headers.insert("x-twilio-signature", signature.parse().unwrap());

        let response = initialize_voice_socket(State(state.clone()), headers, body.to_owned())
            .await
            .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/xml"
        );
        assert!(state.twilio.contains_key("CA123"));

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("<Connect>"));
        assert!(body.contains("wss://api.voxagent.in/bridge/twilio/voice/stream"));
    }
}
