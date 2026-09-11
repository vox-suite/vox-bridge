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
}

impl AssemblyAiStt {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            endpoint: "wss://streaming.assemblyai.com/v3/ws".into(),
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
            "{}?speech_model={}&encoding=pcm_mulaw&sample_rate=8000",
            self.endpoint, self.model
        );
        let mut request = url
            .into_client_request()
            .map_err(|_| provider_error("invalid streaming endpoint"))?;
        let authorization = HeaderValue::from_str(&self.api_key)
            .map_err(|_| VoiceError::Configuration("ASSEMBLYAI_API_KEY is invalid".into()))?;
        request
            .headers_mut()
            .insert(AUTHORIZATION, authorization);
        Ok(request)
    }
}

pub struct AssemblyAiSession {
    sender: Mutex<SplitSink<AssemblySocket, Message>>,
    receiver: Mutex<SplitStream<AssemblySocket>>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AssemblyEvent {
    Turn {
        end_of_turn: bool,
        transcript: String,
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
            Ok(Some(SttEvent::FinalTranscript(transcript.trim().to_owned())))
        }
        AssemblyEvent::SpeechStarted => Ok(Some(SttEvent::SpeechStarted)),
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
        }))
    }
}

#[async_trait]
impl SttSession for AssemblyAiSession {
    async fn send_audio(&self, audio: Bytes) -> Result<(), VoiceError> {
        self.sender
            .lock()
            .await
            .send(audio_message(audio))
            .await
            .map_err(|_| provider_error("audio send failed"))
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
        let mut sender = self.sender.lock().await;
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
        let final_turn = r#"{"type":"Turn","turn_order":1,"end_of_turn":true,"transcript":"Book a table"}"#;
        let partial = r#"{"type":"Turn","turn_order":1,"end_of_turn":false,"transcript":"Book"}"#;
        let empty = r#"{"type":"Turn","turn_order":2,"end_of_turn":true,"transcript":"   "}"#;

        assert_eq!(
            parse_event(final_turn).unwrap(),
            Some(SttEvent::FinalTranscript("Book a table".into()))
        );
        assert_eq!(parse_event(partial).unwrap(), None);
        assert_eq!(parse_event(empty).unwrap(), None);
    }

    #[test]
    fn emits_speech_started() {
        let event = r#"{"type":"SpeechStarted","timestamp":40}"#;

        assert_eq!(parse_event(event).unwrap(), Some(SttEvent::SpeechStarted));
    }

    #[test]
    fn sends_audio_as_unchanged_binary_data() {
        let audio = Bytes::from_static(&[0xff, 0x7f]);

        assert_eq!(audio_message(audio.clone()), Message::Binary(audio));
    }
}
