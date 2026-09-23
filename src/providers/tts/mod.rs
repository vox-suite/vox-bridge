/**
* this file code contains text to speech provider traits and types
*/
pub mod elevenlabs;
pub mod sarvam;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use std::pin::Pin;

use crate::voice::provider::VoiceError;

pub use elevenlabs::ElevenLabsTts;
pub use sarvam::SarvamTts;

pub type AudioStream = Pin<Box<dyn Stream<Item = Result<Bytes, VoiceError>> + Send>>;

#[async_trait]
pub trait TtsProvider: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<AudioStream, VoiceError>;
}
