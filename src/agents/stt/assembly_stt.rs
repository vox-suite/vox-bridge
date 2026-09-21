use crate::voice::provider::{SttEvent, SttProvider, SttSession, VoiceError};
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

type AssemblySocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct AssemblyAiStt {
    api_key: String,
    model: String,
    endpoint: String,
    min_turn_silence: u32,
    max_turn_silence: u32,
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

        Self {
            api_key,
            model,
            endpoint: "wss://streaming.assemblyai.com/v3/ws".into(),
            min_turn_silence,
            max_turn_silence,
        }
    }

    fn request(&self) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, VoiceError> {
        if !self
            .model
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return Err(VoiceError::Configuration(
                "ASSEMBLYAI_SPEECH_MODEL contains unsupported characters".into(),
            ));
        }
        let url = format!(
            "{}?speech_model={}&encoding=pcm_mulaw&sample_rate=8000&mode=min_latency&min_turn_silence={}&max_turn_silence={}&voice_focus=near-field",
            self.endpoint, self.model, self.min_turn_silence, self.max_turn_silence
        );
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

const ASSEMBLY_BATCH_BYTES: usize = 800;
const ASSEMBLY_MINIMUM_BYTES: usize = 400;

#[derive(Default)]
struct AudioBatcher {
    pending: Vec<u8>,
}

impl AudioBatcher {
    fn push(&mut self, audio: Bytes) -> Vec<Bytes> {
        self.pending.extend_from_slice(&audio);
        let mut batches = Vec::new();
        while self.pending.len() >= ASSEMBLY_BATCH_BYTES {
            let remainder = self.pending.split_off(ASSEMBLY_BATCH_BYTES);
            let batch = std::mem::replace(&mut self.pending, remainder);
            batches.push(Bytes::from(batch));
        }
        batches
    }

    fn finish(&mut self) -> Option<Bytes> {
        if self.pending.len() < ASSEMBLY_MINIMUM_BYTES {
            self.pending.clear();
            return None;
        }
        Some(Bytes::from(std::mem::take(&mut self.pending)))
    }
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AssemblyEvent {
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

fn parse_event(raw: &str) -> Result<Option<SttEvent>, VoiceError> {
    match serde_json::from_str::<AssemblyEvent>(raw)
        .map_err(|_| provider_error("invalid streaming event"))?
    {
        AssemblyEvent::Turn {
            end_of_turn: true,
            transcript,
        } if !transcript.trim().is_empty() => {
            let text = transcript.trim().to_owned();
            tracing::info!(transcript = %text, "STT: Final user transcript received");
            Ok(Some(SttEvent::FinalTranscript(text)))
        }
        AssemblyEvent::SpeechStarted => {
            tracing::info!("STT: User speech started");
            Ok(Some(SttEvent::SpeechStarted))
        }
        AssemblyEvent::Error { error_code, error } => Err(VoiceError::Provider {
            provider: "assemblyai",
            message: format!("streaming error {error_code}: {error}"),
        }),
        AssemblyEvent::Turn { end_of_turn: false, transcript } if !transcript.trim().is_empty() => {
            Ok(Some(SttEvent::PartialTranscript(transcript.trim().to_owned())))
        }
        AssemblyEvent::Turn { .. } | AssemblyEvent::Other => Ok(None),
    }
}

fn audio_message(audio: Bytes) -> Message {
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
        Ok(Arc::new(AssemblyAiSession {
            sender: Mutex::new(sender),
            receiver: Mutex::new(receiver),
            audio: Mutex::new(AudioBatcher::default()),
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
    use crate::voice::provider::SttEvent;
    use bytes::Bytes;

    #[test]
    fn emits_only_final_non_empty_turns() {
        let final_turn =
            r#"{"type":"Turn","turn_order":1,"end_of_turn":true,"transcript":"Book a table"}"#;
        let partial = r#"{"type":"Turn","turn_order":1,"end_of_turn":false,"transcript":"Book"}"#;
        let empty = r#"{"type":"Turn","turn_order":2,"end_of_turn":true,"transcript":"   "}"#;

        assert_eq!(
            parse_event(final_turn).unwrap(),
            Some(SttEvent::FinalTranscript("Book a table".into()))
        );
        assert_eq!(parse_event(partial).unwrap(), Some(SttEvent::PartialTranscript("Book".into())));
        assert_eq!(parse_event(empty).unwrap(), None);
    }

    #[test]
    fn emits_speech_started() {
        let event = r#"{"type":"SpeechStarted","timestamp":40}"#;

        assert_eq!(parse_event(event).unwrap(), Some(SttEvent::SpeechStarted));
    }

    #[test]
    fn batches_twilio_frames_into_assembly_compliant_audio() {
        let mut batcher = AudioBatcher::default();

        for _ in 0..4 {
            assert!(batcher.push(Bytes::from(vec![0xff; 160])).is_empty());
        }

        assert_eq!(
            batcher.push(Bytes::from(vec![0xff; 160])),
            vec![Bytes::from(vec![0xff; 800])]
        );
    }

    #[test]
    fn flushes_a_valid_partial_audio_batch() {
        let mut batcher = AudioBatcher::default();

        assert!(batcher.push(Bytes::from(vec![0xff; 480])).is_empty());

        assert_eq!(batcher.finish(), Some(Bytes::from(vec![0xff; 480])));
    }

    #[test]
    fn drops_an_audio_batch_below_assembly_minimum() {
        let mut batcher = AudioBatcher::default();

        assert!(batcher.push(Bytes::from(vec![0xff; 320])).is_empty());

        assert_eq!(batcher.finish(), None);
    }

    #[test]
    fn surfaces_streaming_error_events() {
        let event = r#"{"type":"Error","error_code":3007,"error":"Input Duration Error"}"#;

        let error = parse_event(event).unwrap_err();

        assert!(error.to_string().contains("3007"));
        assert!(error.to_string().contains("Input Duration Error"));
    }
}
