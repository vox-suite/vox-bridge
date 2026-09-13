use crate::voice::{
    context::CallContext,
    provider::{AgentProvider, VoiceError},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub struct CoreAgentClient {
    client: reqwest::Client,
    endpoint: String,
    service_token: String,
}

#[derive(Serialize)]
struct RespondRequest<'a> {
    identity: Identity<'a>,
    external_conversation_id: &'a str,
    text: &'a str,
    initiation_context: Option<&'a str>,
}

#[derive(Serialize)]
struct Identity<'a> {
    channel: &'a str,
    external_id: &'a str,
}

#[derive(Deserialize)]
struct RespondResponse {
    text: String,
}

impl CoreAgentClient {
    pub fn new(base_url: String, service_token: String) -> Result<Self, VoiceError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| configuration("Core HTTP client creation failed"))?;
        Ok(Self {
            client,
            endpoint: format!(
                "{}/v1/conversations/respond",
                base_url.trim_end_matches('/')
            ),
            service_token,
        })
    }
}

#[async_trait]
impl AgentProvider for CoreAgentClient {
    async fn respond(&self, context: &CallContext, transcript: &str) -> Result<String, VoiceError> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.service_token)
            .json(&RespondRequest {
                identity: Identity {
                    channel: &context.channel,
                    external_id: &context.external_identity,
                },
                external_conversation_id: &context.external_conversation_id,
                text: transcript,
                initiation_context: context.initiation_context.as_deref(),
            })
            .send()
            .await
            .map_err(|_| provider_error("Core request failed"))?;
        if !response.status().is_success() {
            return Err(provider_error("Core returned an error"));
        }
        response
            .json::<RespondResponse>()
            .await
            .map(|body| body.text)
            .map_err(|_| provider_error("Core returned an invalid response"))
    }
}

fn configuration(message: &str) -> VoiceError {
    VoiceError::Configuration(message.into())
}

fn provider_error(message: &str) -> VoiceError {
    VoiceError::Provider {
        provider: "vox-core",
        message: message.into(),
    }
}
