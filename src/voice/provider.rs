use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use std::pin::Pin;
use thiserror::Error;

pub type AudioStream = Pin<Box<dyn Stream<Item = Result<Bytes, VoiceError>> + Send>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttEvent {
    SpeechStarted,
    FinalTranscript(String),
}

#[derive(Debug, Error)]
pub enum VoiceError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("{provider} provider error: {message}")]
    Provider {
        provider: &'static str,
        message: String,
    },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("{0} timed out")]
    Timeout(&'static str),
}

#[async_trait]
pub trait SttSession: Send {
    async fn send_audio(&mut self, audio: Bytes) -> Result<(), VoiceError>;
    async fn next_event(&mut self) -> Result<Option<SttEvent>, VoiceError>;
    async fn finish(&mut self) -> Result<(), VoiceError>;
}

#[async_trait]
pub trait SttProvider: Send + Sync {
    async fn connect(&self) -> Result<Box<dyn SttSession>, VoiceError>;
}

#[async_trait]
pub trait AgentProvider: Send + Sync {
    async fn respond(&self, transcript: &str) -> Result<String, VoiceError>;
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<AudioStream, VoiceError>;
}
