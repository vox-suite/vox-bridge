/**
* this file code contains core conversation client interfaces and types
*/
pub mod client;
pub mod protocol;
pub mod sse;

use async_trait::async_trait;
use futures_util::Stream;
use std::pin::Pin;

use crate::channels::context::CallContext;
use crate::voice::provider::VoiceError;

pub use client::CoreClient;

pub type TextStream = Pin<Box<dyn Stream<Item = Result<String, VoiceError>> + Send>>;
pub type ConversationEventStream =
    Pin<Box<dyn Stream<Item = Result<ConversationEvent, VoiceError>> + Send>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationEvent {
    Text(String),
    LookupPending,
}

pub type AgentEvent = ConversationEvent;
pub type AgentEventStream = ConversationEventStream;

#[async_trait]
pub trait ConversationClient: Send + Sync {
    async fn respond(&self, context: &CallContext, transcript: &str) -> Result<String, VoiceError>;

    async fn respond_stream(
        &self,
        context: &CallContext,
        transcript: &str,
    ) -> Result<TextStream, VoiceError> {
        let text = self.respond(context, transcript).await?;
        Ok(Box::pin(futures_util::stream::once(
            async move { Ok(text) },
        )))
    }

    async fn respond_events(
        &self,
        context: &CallContext,
        transcript: &str,
    ) -> Result<ConversationEventStream, VoiceError> {
        use futures_util::StreamExt;
        Ok(Box::pin(
            self.respond_stream(context, transcript)
                .await?
                .map(|result| result.map(ConversationEvent::Text)),
        ))
    }

    async fn speculate(&self, _context: &CallContext, _transcript: &str) -> Result<(), VoiceError> {
        Ok(())
    }

    async fn complete(&self, _context: &CallContext) -> Result<(), VoiceError> {
        Ok(())
    }
}

pub use ConversationClient as AgentProvider;
