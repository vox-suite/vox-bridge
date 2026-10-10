/**
* this file code contains assemblyai realtime speech to text provider
* upgraded with AssemblyAI Streaming v3: Word Boost (Custom Vocabulary)
* and multi-rate audio format support (8kHz mulaw & 16kHz linear PCM).
*/
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};

use crate::providers::stt::{SttEvent, SttProvider, SttSession};
use crate::voice::provider::VoiceError;

type AssemblySocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct AssemblyAiStt {
    api_key: String,
    model: String,
    endpoint: String,
    min_turn_silence: u32,
    max_turn_silence: u32,
    word_boost: Vec<String>,
    encoding: String,
    sample_rate: u32,
}

impl AssemblyAiStt {
    pub fn new(api_key: String, model: String) -> Self {
        let min_turn_silence = std::env::var("ASSEMBLYAI_MIN_TURN_SILENCE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(200);
        let max_turn_silence = std::env::var("ASSEMBLYAI_MAX_TURN_SILENCE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400);

        let word_boost = std::env::var("ASSEMBLYAI_WORD_BOOST")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_else(|_| {
                vec![
                    "Vox".into(),
                    "Spans".into(),
                    "Proposal".into(),
                    "Authorize".into(),
                    "MapLibre".into(),
                    "Zomato".into(),
                    "Expedia".into(),
                    "Uber".into(),
                    "Twilio".into(),
                    "Railway".into(),
                    "Supabase".into(),
                ]
            });

        Self {
            api_key,
            model,
            endpoint: "wss://streaming.assemblyai.com/v3/ws".into(),
            min_turn_silence,
            max_turn_silence,
            word_boost,
            encoding: "pcm_mulaw".into(),
            sample_rate: 8000,
        }
    }

    /// Set custom vocabulary / word boost terms for AssemblyAI acoustic & language model biasing.
    pub fn with_word_boost(mut self, words: Vec<String>) -> Self {
        self.word_boost = words;
        self
    }

    /// Configure the audio encoding and sample rate (e.g. 16kHz pcm_s16le for desktop, 8kHz pcm_mulaw for telephony).
    pub fn with_audio_format(mut self, encoding: impl Into<String>, sample_rate: u32) -> Self {
        self.encoding = encoding.into();
        self.sample_rate = sample_rate;
        self
    }

    pub fn word_boost(&self) -> &[String] {
        &self.word_boost
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn encoding(&self) -> &str {
        &self.encoding
    }

    pub fn build_url(&self) -> Result<String, VoiceError> {
        if !self
            .model
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return Err(VoiceError::Configuration(
                "speech model contains unsupported characters".into(),
            ));
        }

        let mut url = format!(
            "{}?speech_model={}&encoding={}&sample_rate={}&mode=min_latency&min_turn_silence={}&max_turn_silence={}&voice_focus=near-field",
            self.endpoint,
            self.model,
            self.encoding,
            self.sample_rate,
            self.min_turn_silence,
            self.max_turn_silence
        );

        if !self.word_boost.is_empty()
            && let Ok(boost_json) = serde_json::to_string(&self.word_boost)
            && let Ok(encoded) = serde_urlencoded::to_string([("word_boost", &boost_json)])
        {
            url.push('&');
            url.push_str(&encoded);
        }

        Ok(url)
    }

    fn request(&self) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, VoiceError> {
        let url = self.build_url()?;
        let mut request = url
            .into_client_request()
            .map_err(|_| provider_error("invalid streaming endpoint"))?;
        let authorization = HeaderValue::from_str(&self.api_key)
            .map_err(|_| VoiceError::Configuration("ASSEMBLYAI_API_KEY is invalid".into()))?;
        request.headers_mut().insert(AUTHORIZATION, authorization);
        Ok(request)
    }
}

pub struct AssemblyAiSession {
    sender: Mutex<SplitSink<AssemblySocket, Message>>,
    receiver: Mutex<SplitStream<AssemblySocket>>,
    audio: Mutex<AudioBatcher>,
}

pub const ASSEMBLY_BATCH_BYTES: usize = 800;
pub const ASSEMBLY_MINIMUM_BYTES: usize = 400;

pub struct AudioBatcher {
    pub pending: Vec<u8>,
    pub batch_bytes: usize,
    pub min_bytes: usize,
}

impl Default for AudioBatcher {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            batch_bytes: ASSEMBLY_BATCH_BYTES,
            min_bytes: ASSEMBLY_MINIMUM_BYTES,
        }
    }
}

impl AudioBatcher {
    pub fn new(batch_bytes: usize, min_bytes: usize) -> Self {
        Self {
            pending: Vec::new(),
            batch_bytes,
            min_bytes,
        }
    }

    pub fn push(&mut self, audio: Bytes) -> Vec<Bytes> {
        self.pending.extend_from_slice(&audio);
        let mut batches = Vec::new();
        while self.pending.len() >= self.batch_bytes {
            let remainder = self.pending.split_off(self.batch_bytes);
            let batch = std::mem::replace(&mut self.pending, remainder);
            batches.push(Bytes::from(batch));
        }
        batches
    }

    pub fn finish(&mut self) -> Option<Bytes> {
        if self.pending.len() < self.min_bytes {
            self.pending.clear();
            return None;
        }
        Some(Bytes::from(std::mem::take(&mut self.pending)))
    }
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum AssemblyEvent {
    Turn {
        end_of_turn: bool,
        transcript: String,
    },
    Error {
        error_code: u16,
        error: String,
    },
    SpeechStarted,
    #[serde(other)]
    Other,
}

pub fn parse_event(raw: &str) -> Result<Option<SttEvent>, VoiceError> {
    match serde_json::from_str::<AssemblyEvent>(raw)
        .map_err(|_| provider_error("invalid streaming event"))?
    {
        AssemblyEvent::Turn {
            end_of_turn: true,
            transcript,
        } if !transcript.trim().is_empty() => {
            let text = transcript.trim().to_owned();
            tracing::info!(
                transcript_bytes = text.len(),
                "STT: Final user transcript received (AssemblyAI)"
            );
            Ok(Some(SttEvent::FinalTranscript(text)))
        }
        AssemblyEvent::SpeechStarted => {
            tracing::info!("STT: User speech started (AssemblyAI)");
            Ok(Some(SttEvent::SpeechStarted))
        }
        AssemblyEvent::Error { error_code, error } => Err(VoiceError::Provider {
            provider: "assemblyai",
            message: format!("streaming error {error_code}: {error}"),
        }),
        AssemblyEvent::Turn {
            end_of_turn: false,
            transcript,
        } if !transcript.trim().is_empty() => Ok(Some(SttEvent::PartialTranscript(
            transcript.trim().to_owned(),
        ))),
        AssemblyEvent::Turn { .. } | AssemblyEvent::Other => Ok(None),
    }
}

pub fn audio_message(audio: Bytes) -> Message {
    Message::Binary(audio)
}

fn provider_error(message: &str) -> VoiceError {
    VoiceError::Provider {
        provider: "assemblyai",
        message: message.into(),
    }
}

#[async_trait]
impl SttProvider for AssemblyAiStt {
    async fn connect(&self) -> Result<Arc<dyn SttSession>, VoiceError> {
        let request = self.request()?;
        let (socket, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(request))
            .await
            .map_err(|_| VoiceError::Timeout("AssemblyAI connection"))?
            .map_err(|_| provider_error("streaming connection failed"))?;
        let (sender, receiver) = socket.split();

        let (batch_bytes, min_bytes) = if self.sample_rate == 16000 && self.encoding == "pcm_s16le"
        {
            (3200, 1600)
        } else {
            (ASSEMBLY_BATCH_BYTES, ASSEMBLY_MINIMUM_BYTES)
        };

        Ok(Arc::new(AssemblyAiSession {
            sender: Mutex::new(sender),
            receiver: Mutex::new(receiver),
            audio: Mutex::new(AudioBatcher::new(batch_bytes, min_bytes)),
        }))
    }
}

#[async_trait]
impl SttSession for AssemblyAiSession {
    async fn send_audio(&self, audio: Bytes) -> Result<(), VoiceError> {
        let batches = self.audio.lock().await.push(audio);
        let mut sender = self.sender.lock().await;
        for batch in batches {
            sender
                .send(audio_message(batch))
                .await
                .map_err(|_| provider_error("audio send failed"))?;
        }
        Ok(())
    }

    async fn force_endpoint(&self) -> Result<(), VoiceError> {
        let mut sender = self.sender.lock().await;
        sender
            .send(Message::Text(r#"{"type":"ForceEndpoint"}"#.into()))
            .await
            .map_err(|_| provider_error("endpoint send failed"))
    }

    async fn next_event(&self) -> Result<Option<SttEvent>, VoiceError> {
        loop {
            match self.receiver.lock().await.next().await {
                Some(Ok(Message::Text(raw))) => {
                    if let Some(event) = parse_event(raw.as_str())? {
                        return Ok(Some(event));
                    }
                }
                Some(Ok(Message::Close(_))) | None => return Ok(None),
                Some(Ok(_)) => {}
                Some(Err(_)) => return Err(provider_error("streaming receive failed")),
            }
        }
    }

    async fn finish(&self) -> Result<(), VoiceError> {
        let pending = self.audio.lock().await.finish();
        let mut sender = self.sender.lock().await;
        if let Some(audio) = pending {
            sender
                .send(audio_message(audio))
                .await
                .map_err(|_| provider_error("audio send failed"))?;
        }
        sender
            .send(Message::Text(r#"{"type":"Terminate"}"#.into()))
            .await
            .map_err(|_| provider_error("termination send failed"))?;
        sender
            .close()
            .await
            .map_err(|_| provider_error("streaming close failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_url_includes_word_boost() {
        let stt = AssemblyAiStt::new("test_key".into(), "universal-3-5-pro".into())
            .with_word_boost(vec!["Vox".into(), "Spans".into()]);
        let url = stt.build_url().expect("URL must build");
        assert!(url.contains("word_boost="));
        assert!(url.contains("universal-3-5-pro"));
        assert!(url.contains("encoding=pcm_mulaw"));
        assert!(url.contains("sample_rate=8000"));
    }

    #[test]
    fn test_build_url_supports_high_fidelity_audio() {
        let stt = AssemblyAiStt::new("test_key".into(), "universal-3-5-pro".into())
            .with_audio_format("pcm_s16le", 16000);
        let url = stt.build_url().expect("URL must build");
        assert!(url.contains("encoding=pcm_s16le"));
        assert!(url.contains("sample_rate=16000"));
    }

    #[test]
    fn test_audio_batcher_custom_chunking() {
        let mut batcher = AudioBatcher::new(3200, 1600);
        let input = Bytes::from(vec![0u8; 7000]);
        let batches = batcher.push(input);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 3200);
        assert_eq!(batches[1].len(), 3200);
        assert_eq!(batcher.pending.len(), 600);

        // Finish with less than min_bytes clears pending
        assert!(batcher.finish().is_none());
    }

    #[test]
    fn test_parse_assemblyai_events() {
        let turn_json =
            r#"{"type":"Turn","end_of_turn":true,"transcript":"Hey Vox schedule my meeting"}"#;
        let event = parse_event(turn_json).unwrap();
        assert_eq!(
            event,
            Some(SttEvent::FinalTranscript(
                "Hey Vox schedule my meeting".into()
            ))
        );

        let partial_json = r#"{"type":"Turn","end_of_turn":false,"transcript":"Hey Vox"}"#;
        let partial_event = parse_event(partial_json).unwrap();
        assert_eq!(
            partial_event,
            Some(SttEvent::PartialTranscript("Hey Vox".into()))
        );

        let started_json = r#"{"type":"SpeechStarted"}"#;
        let started_event = parse_event(started_json).unwrap();
        assert_eq!(started_event, Some(SttEvent::SpeechStarted));
    }
}
