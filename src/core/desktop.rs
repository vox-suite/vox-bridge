use crate::{
    channels::context::CallContext,
    core::{
        ConversationClient, ConversationEvent, ConversationEventStream, TextStream,
        sse::parse_sse_stream,
    },
    voice::provider::VoiceError,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

#[derive(Deserialize)]
pub struct Binding {
    pub session_id: String,
    pub user_id: String,
    pub device_id: Option<String>,
}
#[derive(Clone)]
pub struct DesktopConversationClient {
    client: reqwest::Client,
    base: String,
    token: String,
    session: String,
}
impl DesktopConversationClient {
    pub async fn redeem(
        base: &str,
        token: &str,
        ticket: &str,
    ) -> Result<(Self, Binding), VoiceError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| error("http_setup"))?;
        let response = client
            .post(format!(
                "{}/internal/v1/desktop-voice/redeem",
                base.trim_end_matches('/')
            ))
            .bearer_auth(token)
            .json(&json!({"ticket":ticket}))
            .send()
            .await
            .map_err(|_| error("redemption_unavailable"))?;
        if !response.status().is_success() {
            return Err(error("invalid_or_expired_ticket"));
        }
        let binding: Binding = response
            .json()
            .await
            .map_err(|_| error("invalid_binding"))?;
        Ok((
            Self {
                client,
                base: base.trim_end_matches('/').into(),
                token: token.into(),
                session: binding.session_id.clone(),
            },
            binding,
        ))
    }
}
fn error(message: &str) -> VoiceError {
    VoiceError::Provider {
        provider: "core",
        message: message.into(),
    }
}
#[async_trait]
impl ConversationClient for DesktopConversationClient {
    async fn respond(&self, context: &CallContext, text: &str) -> Result<String, VoiceError> {
        let mut stream = self.respond_stream(context, text).await?;
        let mut result = String::new();
        while let Some(delta) = stream.next().await {
            result.push_str(&delta?);
        }
        Ok(result)
    }
    async fn respond_stream(
        &self,
        context: &CallContext,
        text: &str,
    ) -> Result<TextStream, VoiceError> {
        Ok(Box::pin(
            self.respond_events(context, text)
                .await?
                .map(|r| r.map(|ConversationEvent::Text(t)| t)),
        ))
    }
    async fn respond_events(
        &self,
        context: &CallContext,
        text: &str,
    ) -> Result<ConversationEventStream, VoiceError> {
        let response=self.client.post(format!("{}/internal/v1/desktop-voice/{}/stream",self.base,self.session)).bearer_auth(&self.token).json(&json!({"text":text,"turn_id":context.turn_id,"revision":context.revision,"tts_provider":context.tts_provider,"initiation_context":context.initiation_context})).send().await.map_err(|_|error("conversation_unavailable"))?;
        if !response.status().is_success() {
            return Err(error("conversation_rejected"));
        }
        Ok(Box::pin(parse_sse_stream(response.bytes_stream())))
    }
    async fn complete(&self, _: &CallContext) -> Result<(), VoiceError> {
        self.client
            .post(format!(
                "{}/internal/v1/desktop-voice/{}/complete",
                self.base, self.session
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| error("completion_unavailable"))?
            .error_for_status()
            .map_err(|_| error("completion_rejected"))?;
        Ok(())
    }
}
