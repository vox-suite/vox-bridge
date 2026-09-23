/**
* this file code contains voice provider types and traits
*/
pub use crate::channels::context::CallContext;
pub use crate::core::{
    ConversationClient as AgentProvider, ConversationClient, ConversationEvent as AgentEvent,
    ConversationEvent, ConversationEventStream as AgentEventStream, TextStream,
};
pub use crate::providers::jev::JevClient;
pub use crate::providers::stt::{SttEvent, SttProvider, SttSession};
pub use crate::providers::tts::{AudioStream, TtsProvider};
pub use crate::voice::registry::ProviderSet as VoiceProviders;
pub use crate::voice::registry::ProviderSet;

use thiserror::Error;

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
