/**
* this file code contains speech to text provider traits and event types
*/
pub mod assemblyai;

use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;

use crate::voice::provider::VoiceError;

pub use assemblyai::AssemblyAiStt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttEvent {
    SpeechStarted,
    PartialTranscript(String),
    FinalTranscript(String),
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
