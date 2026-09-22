// this file code contains core client implementation for conversation api

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use std::time::Duration;
use uuid::Uuid;

use crate::channels::context::{CallContext, normalized_e164};
use crate::core::auth::sign_assertion;
use crate::core::protocol::{
    CompleteRequest, HostContext, IdentityPayload, RespondRequest, RespondResponse,
    SpeculateRequest,
};
use crate::core::sse::parse_sse_stream;
use crate::core::{ConversationClient, ConversationEventStream, TextStream};
use crate::voice::provider::VoiceError;

#[derive(Clone)]
pub struct CoreClient {
    client: reqwest::Client,
    base_url: String,
    endpoint: String,
    host_credential_id: Uuid,
    host_audience: String,
    host_secret: String,
    tts_provider: Option<String>,
}

impl CoreClient {
    pub fn new(
        base_url: String,
        host_credential_id: String,
        host_audience: String,
        host_secret: String,
    ) -> Result<Self, VoiceError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|_| {
                VoiceError::Configuration("Core HTTP client creation failed".into())
            })?;
        let trimmed = base_url.trim_end_matches('/').to_owned();
        let host_credential_id = Uuid::parse_str(host_credential_id.trim())
            .map_err(|_| {
                VoiceError::Configuration("VOX_CORE_HOST_CREDENTIAL_ID is invalid".into())
            })?;
        if host_audience.trim().is_empty() || host_secret.trim().is_empty() {
            return Err(VoiceError::Configuration(
                "Core host credential configuration is missing".into(),
            ));
        }
        Ok(Self {
            client,
            endpoint: format!("{trimmed}/v1/conversations/respond"),
            base_url: trimmed,
            host_credential_id,
            host_audience,
            host_secret,
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

    fn host_context(&self, context: &CallContext) -> HostContext<'_> {
        let normalized_phone = normalized_e164(&context.external_identity)
            .unwrap_or_else(|| context.external_identity.clone());
        HostContext {
            host_user_id: format!("{}:{normalized_phone}", context.channel),
            organization_external_key: None,
        }
    }

    fn signed_request(
        &self,
        request: reqwest::RequestBuilder,
        context: &CallContext,
    ) -> Result<reqwest::RequestBuilder, VoiceError> {
        let issued_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| VoiceError::Configuration("system clock is before Unix epoch".into()))?
            .as_secs() as i64;
        let nonce = Uuid::new_v4();
        let host_context = self.host_context(context);
        let signature = sign_assertion(
            &self.host_secret,
            self.host_credential_id,
            &self.host_audience,
            issued_at,
            nonce,
            &host_context.host_user_id,
        )?;
        Ok(request
            .header(
                "X-Vox-Host-Credential",
                self.host_credential_id.to_string(),
            )
            .header("X-Vox-Host-Audience", &self.host_audience)
            .header("X-Vox-Host-Timestamp", issued_at.to_string())
            .header("X-Vox-Host-Nonce", nonce.to_string())
            .header("X-Vox-Host-Signature", signature)
            .header("X-Vox-Host-Secret", &self.host_secret))
    }
}

#[async_trait]
impl ConversationClient for CoreClient {
    async fn respond(&self, context: &CallContext, transcript: &str) -> Result<String, VoiceError> {
        let host_context = self.host_context(context);
        let payload = RespondRequest {
            host_context,
            identity: IdentityPayload {
                channel: &context.channel,
                external_id: &context.external_identity,
            },
            external_conversation_id: &context.external_conversation_id,
            text: transcript,
            initiation_context: context.initiation_context.as_deref(),
            voice_signature: context.voice_signature.as_deref(),
            turn_id: context.turn_id.as_deref(),
            revision: context.revision,
            tts_provider: context
                .tts_provider
                .as_deref()
                .or(self.tts_provider.as_deref()),
        };
        let response = self
            .signed_request(self.client.post(&self.endpoint), context)?
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
        let host_context = self.host_context(context);
        let payload = RespondRequest {
            host_context,
            identity: IdentityPayload {
                channel: &context.channel,
                external_id: &context.external_identity,
            },
            external_conversation_id: &context.external_conversation_id,
            text: transcript,
            initiation_context: context.initiation_context.as_deref(),
            voice_signature: context.voice_signature.as_deref(),
            turn_id: context.turn_id.as_deref(),
            revision: context.revision,
            tts_provider: context
                .tts_provider
                .as_deref()
                .or(self.tts_provider.as_deref()),
        };
        let send_result = self
            .signed_request(self.client.post(&stream_endpoint), context)?
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
            _ => Err(VoiceError::Provider {
                provider: "core",
                message: "Core stream request failed".into(),
            }),
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
        let host_context = self.host_context(context);
        let payload = SpeculateRequest {
            host_context,
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
            .signed_request(
                self.client
                    .post(format!("{}/v1/conversations/speculate", self.base_url)),
                context,
            )?
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
        let host_context = self.host_context(context);
        let payload = CompleteRequest {
            host_context,
            identity: IdentityPayload {
                channel: &context.channel,
                external_id: &context.external_identity,
            },
            external_conversation_id: &context.external_conversation_id,
        };
        let response = self
            .signed_request(self.client.post(&endpoint), context)?
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
