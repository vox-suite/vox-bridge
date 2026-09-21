use crate::voice::provider::{AudioStream, TtsProvider, VoiceError};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Serialize;
use std::time::Duration;

#[derive(Clone)]
pub struct SarvamSettings {
    pub model: String,
    pub language_code: String,
    pub speaker: String,
    pub pace: f64,
}

pub struct SarvamTts {
    http: reqwest::Client,
    api_key: String,
    endpoint: String,
    settings: SarvamSettings,
}

#[derive(Serialize)]
struct SarvamRequest<'a> {
    text: &'a str,
    language_code: &'a str,
    speaker: &'a str,
    model: &'a str,
    pace: f64,
    speech_sample_rate: u32,
    output_audio_codec: &'static str,
}

impl SarvamTts {
    pub fn new(
        http: reqwest::Client,
        api_key: String,
        endpoint: String,
        settings: SarvamSettings,
    ) -> Self {
        Self {
            http,
            api_key,
            endpoint,
            settings,
        }
    }
}

fn provider_error(message: &str) -> VoiceError {
    VoiceError::Provider {
        provider: "sarvam",
        message: message.into(),
    }
}

#[async_trait]
impl TtsProvider for SarvamTts {
    async fn synthesize(&self, text: &str) -> Result<AudioStream, VoiceError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(VoiceError::Protocol("empty TTS input".into()));
        }
        if !trimmed.chars().any(|c| c.is_alphabetic()) {
            tracing::warn!(text = %trimmed, "Skipping Sarvam TTS for text without alphabetic characters");
            return Ok(Box::pin(futures_util::stream::empty()));
        }
        let request = SarvamRequest {
            text: trimmed,
            language_code: &self.settings.language_code,
            speaker: &self.settings.speaker,
            model: &self.settings.model,
            pace: self.settings.pace,
            speech_sample_rate: 8000,
            output_audio_codec: "mulaw",
        };
        let url = format!(
            "{}/text-to-speech/stream",
            self.endpoint.trim_end_matches('/')
        );
        let tts_http_start = std::time::Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            self.http
                .post(url)
                .header("api-subscription-key", &self.api_key)
                .json(&request)
                .send(),
        )
        .await
        .map_err(|_| VoiceError::Timeout("Sarvam response"))?
        .map_err(|err| {
            tracing::error!(error = %err, "Sarvam request send failed");
            provider_error("request failed")
        })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::error!(status = %status, error_body = %body, "Sarvam TTS API returned error");
            return Err(provider_error("request failed"));
        }
        tracing::debug!(
            char_count = text.len(),
            http_latency_ms = tts_http_start.elapsed().as_millis(),
            "Sarvam TTS HTTP response received"
        );
        let stream = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|_| provider_error("audio stream failed")));
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        http::{HeaderMap, StatusCode},
        response::Response,
        routing::post,
    };
    use futures_util::{StreamExt, stream};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    async fn test_server(status: StatusCode) -> (String, Arc<Mutex<Option<(HeaderMap, Value)>>>) {
        let captured = Arc::new(Mutex::new(None));
        let handler_capture = captured.clone();
        let app = Router::new().route(
            "/text-to-speech/stream",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let handler_capture = handler_capture.clone();
                async move {
                    *handler_capture.lock().unwrap() = Some((headers, body));
                    if status.is_success() {
                        Response::builder()
                            .status(status)
                            .body(Body::from_stream(stream::iter([
                                Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(&[
                                    1, 2,
                                ])),
                                Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(&[
                                    3, 4,
                                ])),
                            ])))
                            .unwrap()
                    } else {
                        Response::builder()
                            .status(status)
                            .body(Body::from("sensitive provider response"))
                            .unwrap()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), captured)
    }

    fn settings() -> SarvamSettings {
        SarvamSettings {
            model: "bulbul:v3".into(),
            language_code: "en-IN".into(),
            speaker: "shubh".into(),
            pace: 1.0,
        }
    }

    #[tokio::test]
    async fn streams_mulaw_audio_with_the_selected_profile() {
        let (endpoint, captured) = test_server(StatusCode::OK).await;
        let provider = SarvamTts::new(
            reqwest::Client::new(),
            "sarvam-key".into(),
            endpoint,
            settings(),
        );

        let chunks: Vec<_> = provider
            .synthesize("Hello")
            .await
            .unwrap()
            .map(|chunk| chunk.unwrap().to_vec())
            .collect()
            .await;

        assert_eq!(chunks, vec![vec![1, 2], vec![3, 4]]);
        let guard = captured.lock().unwrap();
        let (headers, body) = guard.as_ref().unwrap();
        assert_eq!(headers["api-subscription-key"], "sarvam-key");
        assert_eq!(
            body,
            &json!({
                "text": "Hello",
                "language_code": "en-IN",
                "speaker": "shubh",
                "model": "bulbul:v3",
                "pace": 1.0,
                "speech_sample_rate": 8000,
                "output_audio_codec": "mulaw"
            })
        );
    }

    #[tokio::test]
    async fn sanitizes_provider_errors() {
        let (endpoint, _) = test_server(StatusCode::UNAUTHORIZED).await;
        let provider = SarvamTts::new(
            reqwest::Client::new(),
            "sarvam-key".into(),
            endpoint,
            settings(),
        );

        let error = provider.synthesize("Hello").await.err().unwrap();

        assert_eq!(error.to_string(), "sarvam provider error: request failed");
        assert!(!error.to_string().contains("sensitive"));
    }

    #[tokio::test]
    async fn skips_text_without_alphabetic_characters() {
        let provider = SarvamTts::new(
            reqwest::Client::new(),
            "sarvam-key".into(),
            "http://127.0.0.1:9".into(),
            settings(),
        );

        let mut stream = provider.synthesize("40.").await.unwrap();
        assert!(stream.next().await.is_none());

        let mut stream2 = provider.synthesize("---").await.unwrap();
        assert!(stream2.next().await.is_none());
    }
}
