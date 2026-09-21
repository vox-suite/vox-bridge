use crate::voice::provider::{AudioStream, TtsProvider, VoiceError};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Serialize;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct ElevenLabsVoiceSettings {
    pub stability: Option<f64>,
    pub similarity_boost: Option<f64>,
    pub speed: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ElevenLabsSettings {
    pub model: String,
    pub voice_id: String,
    pub output_format: String,
    pub voice_settings: Option<ElevenLabsVoiceSettings>,
}

pub struct ElevenLabsTts {
    http: reqwest::Client,
    api_key: String,
    endpoint: String,
    settings: ElevenLabsSettings,
}

#[derive(Serialize)]
struct ElevenLabsRequest<'a> {
    text: &'a str,
    model_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice_settings: Option<ElevenLabsVoiceSettingsRequest>,
}

#[derive(Serialize)]
struct ElevenLabsVoiceSettingsRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    stability: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    similarity_boost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f64>,
}

impl ElevenLabsTts {
    pub fn new(
        http: reqwest::Client,
        api_key: String,
        endpoint: String,
        settings: ElevenLabsSettings,
    ) -> Self {
        Self {
            http,
            api_key,
            endpoint,
            settings,
        }
    }
}

fn provider_error(message: impl Into<String>) -> VoiceError {
    VoiceError::Provider {
        provider: "elevenlabs",
        message: message.into(),
    }
}

#[async_trait]
impl TtsProvider for ElevenLabsTts {
    async fn synthesize(&self, text: &str) -> Result<AudioStream, VoiceError> {
        let trimmed = text.trim();
        if !trimmed.chars().any(|c| c.is_alphabetic()) {
            return Ok(Box::pin(futures_util::stream::empty()));
        }

        let voice_settings = self.settings.voice_settings.as_ref().map(|s| {
            ElevenLabsVoiceSettingsRequest {
                stability: s.stability,
                similarity_boost: s.similarity_boost,
                speed: s.speed,
            }
        });

        let request = ElevenLabsRequest {
            text: trimmed,
            model_id: &self.settings.model,
            voice_settings,
        };

        let url = format!(
            "{}/v1/text-to-speech/{}/stream?output_format={}",
            self.endpoint.trim_end_matches('/'),
            self.settings.voice_id,
            self.settings.output_format,
        );

        let tts_http_start = std::time::Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            self.http
                .post(url)
                .header("xi-api-key", &self.api_key)
                .json(&request)
                .send(),
        )
        .await
        .map_err(|_| VoiceError::Timeout("ElevenLabs response"))?
        .map_err(|err| {
            tracing::error!(error = %err, "ElevenLabs request send failed");
            provider_error("request failed")
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::error!(status = %status, error_body = %body, "ElevenLabs TTS API returned error");
            return Err(provider_error("request failed"));
        }

        tracing::debug!(
            char_count = text.len(),
            http_latency_ms = tts_http_start.elapsed().as_millis(),
            "ElevenLabs TTS HTTP response received"
        );

        let stream = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|_| provider_error("audio stream failed")));

        if self.settings.output_format.starts_with("mp3_") {
            Ok(crate::voice::mp3::transcode_mp3_to_mulaw_stream(stream))
        } else {
            Ok(Box::pin(stream))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Body,
        extract::Path,
        http::{HeaderMap, StatusCode},
        response::Response,
        routing::post,
    };
    use futures_util::{StreamExt, stream};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    async fn test_server(
        status: StatusCode,
    ) -> (String, Arc<Mutex<Option<(String, HeaderMap, Value)>>>) {
        let captured = Arc::new(Mutex::new(None));
        let handler_capture = captured.clone();
        let app = Router::new().route(
            "/v1/text-to-speech/{voice_id}/stream",
            post(
                move |Path(voice_id): Path<String>, headers: HeaderMap, Json(body): Json<Value>| {
                    let handler_capture = handler_capture.clone();
                    async move {
                        *handler_capture.lock().unwrap() = Some((voice_id, headers, body));
                        if status.is_success() {
                            Response::builder()
                                .status(status)
                                .body(Body::from_stream(stream::iter([
                                    Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(&[
                                        10, 20,
                                    ])),
                                    Ok::<_, std::convert::Infallible>(bytes::Bytes::from_static(&[
                                        30, 40,
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
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), captured)
    }

    fn settings() -> ElevenLabsSettings {
        ElevenLabsSettings {
            model: "eleven_flash_v2_5".into(),
            voice_id: "21m00Tcm4TlvDq8ikWAM".into(),
            output_format: "ulaw_8000".into(),
            voice_settings: None,
        }
    }

    #[tokio::test]
    async fn streams_mulaw_audio_with_the_selected_profile() {
        let (endpoint, captured) = test_server(StatusCode::OK).await;
        let provider = ElevenLabsTts::new(
            reqwest::Client::new(),
            "eleven-key".into(),
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

        assert_eq!(chunks, vec![vec![10, 20], vec![30, 40]]);
        let guard = captured.lock().unwrap();
        let (voice_id, headers, body) = guard.as_ref().unwrap();
        assert_eq!(voice_id, "21m00Tcm4TlvDq8ikWAM");
        assert_eq!(headers["xi-api-key"], "eleven-key");
        assert_eq!(
            body,
            &json!({
                "text": "Hello",
                "model_id": "eleven_flash_v2_5"
            })
        );
    }

    #[tokio::test]
    async fn sanitizes_provider_errors() {
        let (endpoint, _) = test_server(StatusCode::UNAUTHORIZED).await;
        let provider = ElevenLabsTts::new(
            reqwest::Client::new(),
            "eleven-key".into(),
            endpoint,
            settings(),
        );

        let error = provider.synthesize("Hello").await.err().unwrap();

        assert_eq!(error.to_string(), "elevenlabs provider error: request failed");
        assert!(!error.to_string().contains("sensitive"));
    }

    #[tokio::test]
    async fn skips_text_without_alphabetic_characters() {
        let provider = ElevenLabsTts::new(
            reqwest::Client::new(),
            "eleven-key".into(),
            "http://127.0.0.1:9".into(),
            settings(),
        );

        let mut stream = provider.synthesize("40.").await.unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn transcodes_mp3_stream_to_mulaw() {
        let sample_mp3 = include_bytes!("../../../tests/fixtures/sample.mp3");
        let app = Router::new().route(
            "/v1/text-to-speech/{voice_id}/stream",
            post(move || async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(sample_mp3.to_vec()))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut tts_settings = settings();
        tts_settings.output_format = "mp3_44100_128".into();
        let provider = ElevenLabsTts::new(
            reqwest::Client::new(),
            "eleven-key".into(),
            format!("http://{address}"),
            tts_settings,
        );

        let mut stream = provider.synthesize("Hello world").await.unwrap();
        let mut total_bytes = 0;
        while let Some(chunk) = stream.next().await {
            total_bytes += chunk.unwrap().len();
        }
        assert!((3500..=5500).contains(&total_bytes));
    }
}
