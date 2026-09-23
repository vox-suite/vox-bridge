/**
* this file code contains elevenlabs text to speech provider
*/
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Serialize;
use std::time::Duration;

use crate::providers::tts::{AudioStream, TtsProvider};
use crate::voice::audio::mp3::transcode_mp3_to_mulaw_stream;
use crate::voice::provider::VoiceError;

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

        let voice_settings =
            self.settings
                .voice_settings
                .as_ref()
                .map(|s| ElevenLabsVoiceSettingsRequest {
                    stability: s.stability,
                    similarity_boost: s.similarity_boost,
                    speed: s.speed,
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
            Ok(transcode_mp3_to_mulaw_stream(stream))
        } else {
            Ok(Box::pin(stream))
        }
    }
}
