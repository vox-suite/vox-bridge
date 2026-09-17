use super::context::CallContext;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use std::{pin::Pin, sync::Arc};
use thiserror::Error;

pub type AudioStream = Pin<Box<dyn Stream<Item = Result<Bytes, VoiceError>> + Send>>;
pub type TextStream = Pin<Box<dyn Stream<Item = Result<String, VoiceError>> + Send>>;

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
pub trait SttSession: Send + Sync {
    async fn send_audio(&self, audio: Bytes) -> Result<(), VoiceError>;
    async fn next_event(&self) -> Result<Option<SttEvent>, VoiceError>;
    async fn finish(&self) -> Result<(), VoiceError>;
}

#[async_trait]
pub trait SttProvider: Send + Sync {
    async fn connect(&self) -> Result<Arc<dyn SttSession>, VoiceError>;
}

#[async_trait]
pub trait AgentProvider: Send + Sync {
    async fn respond(&self, context: &CallContext, transcript: &str) -> Result<String, VoiceError>;
    async fn respond_stream(
        &self,
        context: &CallContext,
        transcript: &str,
    ) -> Result<TextStream, VoiceError> {
        let text = self.respond(context, transcript).await?;
        Ok(Box::pin(futures_util::stream::once(async move { Ok(text) })))
    }
    async fn complete(&self, _context: &CallContext) -> Result<(), VoiceError> {
        Ok(())
    }
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<AudioStream, VoiceError>;
}
