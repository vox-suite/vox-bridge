/**
* this file code contains core client implementation for conversation api
*/
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use std::time::Duration;

use crate::channels::context::CallContext;
use crate::core::protocol::{
    CompleteRequest, IdentityPayload, RespondRequest, RespondResponse, SpeculateRequest,
};
use crate::core::sse::parse_sse_stream;
use crate::core::{ConversationClient, ConversationEventStream, TextStream};
use crate::voice::provider::VoiceError;

#[derive(Clone)]
pub struct CoreClient {
    client: reqwest::Client,
    base_url: String,
    endpoint: String,
    auth_token: String,
    tts_provider: Option<String>,
}

impl CoreClient {
    pub fn new(base_url: String, auth_token: String) -> Result<Self, VoiceError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|_| VoiceError::Configuration("Core HTTP client creation failed".into()))?;
        let trimmed = base_url.trim_end_matches('/').to_owned();
        if auth_token.trim().is_empty() {
            return Err(VoiceError::Configuration("VOX_AUTH_TOKEN is missing".into()));
        }
        Ok(Self {
            client,
            endpoint: format!("{trimmed}/v1/conversations/respond"),
            base_url: trimmed,
            auth_token,
            tts_provider: None,
        })
    }

    pub fn with_tts_provider(mut self, provider: impl Into<String>) -> Self {
        self.tts_provider = Some(provider.into());
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.auth_token)
    }
}

#[async_trait]
impl ConversationClient for CoreClient {
    async fn respond(&self, context: &CallContext, transcript: &str) -> Result<String, VoiceError> {
        let payload = RespondRequest {
            identity: IdentityPayload {
                channel: &context.channel,
                external_id: &context.external_identity,
            },
            external_conversation_id: &context.external_conversation_id,
            text: transcript,
            initiation_context: context.initiation_context.as_deref(),
            turn_id: context.turn_id.as_deref(),
            revision: context.revision,
            tts_provider: context
                .tts_provider
                .as_deref()
                .or(self.tts_provider.as_deref()),
            filler: context.filler.as_deref(),
        };
        let response = self
            .authorized(self.client.post(&self.endpoint))
            .json(&payload)
            .send()
            .await
            .map_err(|_| VoiceError::Provider {
                provider: "core",
                message: "Core request failed".into(),
            })?;
        if !response.status().is_success() {
            return Err(VoiceError::Provider {
                provider: "core",
                message: "Core returned an error".into(),
            });
        }
        response
            .json::<RespondResponse>()
            .await
            .map(|body| body.text)
            .map_err(|_| VoiceError::Provider {
                provider: "core",
                message: "Core returned an invalid response".into(),
            })
    }

    async fn respond_events(
        &self,
        context: &CallContext,
        transcript: &str,
    ) -> Result<ConversationEventStream, VoiceError> {
        let stream_endpoint = format!("{}/v1/conversations/respond/stream", self.base_url);
        let start_time = std::time::Instant::now();
        let payload = RespondRequest {
            identity: IdentityPayload {
                channel: &context.channel,
                external_id: &context.external_identity,
            },
            external_conversation_id: &context.external_conversation_id,
            text: transcript,
            initiation_context: context.initiation_context.as_deref(),
            turn_id: context.turn_id.as_deref(),
            revision: context.revision,
            tts_provider: context
                .tts_provider
                .as_deref()
                .or(self.tts_provider.as_deref()),
            filler: context.filler.as_deref(),
        };
        let send_result = self
            .authorized(self.client.post(&stream_endpoint))
            .header("Accept", "text/event-stream")
            .json(&payload)
            .send()
            .await;

        match send_result {
            Ok(response) if response.status().is_success() => {
                tracing::info!(
                    conversation_id = %context.external_conversation_id,
                    endpoint = %stream_endpoint,
                    connect_time_ms = start_time.elapsed().as_millis(),
                    "CoreClient: Stream connection established"
                );
                let stream = response.bytes_stream();
                Ok(Box::pin(parse_sse_stream(stream)))
            }
            Ok(response)
                if response.status() == reqwest::StatusCode::NOT_FOUND
                    || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED =>
            {
                tracing::warn!(
                    endpoint = %stream_endpoint,
                    elapsed_ms = start_time.elapsed().as_millis(),
                    "CoreClient: Stream unavailable, falling back to unary respond"
                );
                let text = self.respond(context, transcript).await?;
                Ok(Box::pin(stream::once(async move {
                    Ok(crate::core::ConversationEvent::Text(text))
                })))
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                tracing::warn!(
                    conversation_id = %context.external_conversation_id,
                    %status,
                    body = %body.chars().take(500).collect::<String>(),
                    "CoreClient: Stream request rejected"
                );
                Err(VoiceError::Provider {
                    provider: "core",
                    message: format!("Core stream request failed ({status})"),
                })
            }
            Err(error) => {
                tracing::warn!(
                    conversation_id = %context.external_conversation_id,
                    %error,
                    "CoreClient: Stream transport failed"
                );
                Err(VoiceError::Provider {
                    provider: "core",
                    message: "Core stream request failed".into(),
                })
            }
        }
    }

    async fn respond_stream(
        &self,
        context: &CallContext,
        transcript: &str,
    ) -> Result<TextStream, VoiceError> {
        Ok(Box::pin(
            self.respond_events(context, transcript)
                .await?
                .filter_map(|item| async {
                    match item {
                        Ok(crate::core::ConversationEvent::Text(text)) => Some(Ok(text)),
                        Ok(crate::core::ConversationEvent::LookupPending) => None,
                        Err(err) => Some(Err(err)),
                    }
                }),
        ))
    }

    async fn speculate(&self, context: &CallContext, transcript: &str) -> Result<(), VoiceError> {
        let payload = SpeculateRequest {
            identity: IdentityPayload {
                channel: &context.channel,
                external_id: &context.external_identity,
            },
            external_conversation_id: &context.external_conversation_id,
            text: transcript,
            turn_id: context.turn_id.as_deref(),
            revision: context.revision,
            tts_provider: context
                .tts_provider
                .as_deref()
                .or(self.tts_provider.as_deref()),
        };
        let response = self
            .authorized(
                self.client
                    .post(format!("{}/v1/conversations/speculate", self.base_url)),
            )
            .timeout(Duration::from_secs(6))
            .json(&payload)
            .send()
            .await
            .map_err(|_| VoiceError::Provider {
                provider: "core",
                message: "Core speculation failed".into(),
            })?;
        if !response.status().is_success() {
            return Err(VoiceError::Provider {
                provider: "core",
                message: "Core speculation unavailable".into(),
            });
        }
        Ok(())
    }

    async fn complete(&self, context: &CallContext) -> Result<(), VoiceError> {
        let endpoint = format!("{}/v1/conversations/complete", self.base_url);
        let payload = CompleteRequest {
            identity: IdentityPayload {
                channel: &context.channel,
                external_id: &context.external_identity,
            },
            external_conversation_id: &context.external_conversation_id,
        };
        let response = self
            .authorized(self.client.post(&endpoint))
            .json(&payload)
            .send()
            .await
            .map_err(|_| VoiceError::Provider {
                provider: "core",
                message: "Core complete request failed".into(),
            })?;
        if !response.status().is_success() {
            return Err(VoiceError::Provider {
                provider: "core",
                message: "Core returned an error on complete".into(),
            });
        }
        Ok(())
    }
}
