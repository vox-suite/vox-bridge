/**
* this file code contains sarvam text to speech provider
*/
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Serialize;
use std::time::Duration;

use crate::providers::tts::{AudioStream, TtsProvider};
use crate::voice::provider::VoiceError;

#[derive(Clone, Debug, PartialEq)]
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
