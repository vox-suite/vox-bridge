/**
* this file code contains core client implementation for conversation api, durable tasks, and proposals
*/
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::Duration;
use uuid::Uuid;

use crate::channels::context::CallContext;
use crate::core::protocol::{
    ApproveRequestBody, CompleteRequest, ContextRequest, CreateProposalRequest, DurableTask,
    HostContextRequest, IdentityPayload, Proposal, ProposeRequestBody, RespondRequest,
    RespondResponse, SpeculateRequest, StartTaskRequest, StartTaskRequestBody, WaitRequest,
    WaitRequestBody,
};
use crate::core::sse::parse_sse_stream;
use crate::core::{ConversationClient, ConversationEventStream, TextStream};
use crate::voice::provider::VoiceError;

type HmacSha256 = Hmac<Sha256>;

pub fn canonical_assertion(
    credential_id: Uuid,
    audience: &str,
    issued_at: i64,
    nonce: Uuid,
    host_user_id: &str,
    organization_external_key: Option<&str>,
) -> String {
    let mut canonical = String::from("vox-host-assertion-v1");
    for field in [
        credential_id.to_string(),
        audience.to_owned(),
        issued_at.to_string(),
        nonce.to_string(),
        host_user_id.to_owned(),
        organization_external_key.unwrap_or_default().to_owned(),
    ] {
        canonical.push('|');
        canonical.push_str(&field.len().to_string());
        canonical.push(':');
        canonical.push_str(&field);
    }
    canonical
}

#[derive(Clone)]
pub struct CoreClient {
    client: reqwest::Client,
    base_url: String,
    endpoint: String,
    auth_token: String,
    tts_provider: Option<String>,
    host_credential_id: Option<Uuid>,
    host_audience: Option<String>,
    host_secret: Option<String>,
}

impl CoreClient {
    pub fn new(base_url: String, auth_token: String) -> Result<Self, VoiceError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|_| VoiceError::Configuration("Core HTTP client creation failed".into()))?;
        let trimmed = base_url.trim_end_matches('/').to_owned();
        if auth_token.trim().is_empty() {
            return Err(VoiceError::Configuration(
                "VOX_AUTH_TOKEN is missing".into(),
            ));
        }
        Ok(Self {
            client,
            endpoint: format!("{trimmed}/v1/conversations/respond"),
            base_url: trimmed,
            auth_token,
            tts_provider: None,
            host_credential_id: None,
            host_audience: None,
            host_secret: None,
        })
    }

    pub fn with_tts_provider(mut self, provider: impl Into<String>) -> Self {
        self.tts_provider = Some(provider.into());
        self
    }

    pub fn with_host_trust(
        mut self,
        credential_id: Uuid,
        audience: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        self.host_credential_id = Some(credential_id);
        self.host_audience = Some(audience.into());
        self.host_secret = Some(secret.into());
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.auth_token)
    }

    fn prepare_request(
        &self,
        mut request: reqwest::RequestBuilder,
        host_user_id: &str,
        organization_key: Option<&str>,
    ) -> Result<reqwest::RequestBuilder, VoiceError> {
        request = self.authorized(request);
        if let (Some(cred_id), Some(audience), Some(secret)) = (
            self.host_credential_id,
            self.host_audience.as_deref(),
            self.host_secret.as_deref(),
        ) {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let nonce = Uuid::new_v4();
            let canonical = canonical_assertion(
                cred_id,
                audience,
                now_secs,
                nonce,
                host_user_id,
                organization_key,
            );
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
                .map_err(|_| VoiceError::Configuration("Failed to create HMAC signer".into()))?;
            mac.update(canonical.as_bytes());
            let signature = hex::encode(mac.finalize().into_bytes());

            request = request
                .header("x-vox-host-credential", cred_id.to_string())
                .header("x-vox-host-secret", secret)
                .header("x-vox-host-audience", audience)
                .header("x-vox-host-timestamp", now_secs.to_string())
                .header("x-vox-host-nonce", nonce.to_string())
                .header("x-vox-host-signature", signature);
        }
        Ok(request)
    }

    pub async fn get_durable_task(
        &self,
        context: &CallContext,
        task_id: Uuid,
    ) -> Result<DurableTask, VoiceError> {
        let endpoint = format!("{}/v1/durable-tasks/{task_id}", self.base_url);
        let body = ContextRequest {
            host_context: HostContextRequest {
                host_user_id: context.external_identity.clone(),
                organization_external_key: None,
            },
        };
        let request = self
            .prepare_request(
                self.client.post(&endpoint),
                &context.external_identity,
                None,
            )?
            .json(&body);
        let response = request.send().await.map_err(|err| VoiceError::Provider {
            provider: "core",
            message: format!("Failed to get durable task: {err}"),
        })?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(VoiceError::Provider {
                provider: "core",
                message: format!("Get durable task failed with status {status}"),
            });
        }

        response
            .json::<DurableTask>()
            .await
            .map_err(|err| VoiceError::Provider {
                provider: "core",
                message: format!("Invalid durable task response: {err}"),
            })
    }

    pub async fn start_durable_task(
        &self,
        context: &CallContext,
        task: StartTaskRequest,
    ) -> Result<DurableTask, VoiceError> {
        let endpoint = format!("{}/v1/durable-tasks", self.base_url);
        let body = StartTaskRequestBody {
            host_context: HostContextRequest {
                host_user_id: context.external_identity.clone(),
                organization_external_key: None,
            },
            task,
        };
        let request = self
            .prepare_request(
                self.client.post(&endpoint),
                &context.external_identity,
                None,
            )?
            .json(&body);
        let response = request.send().await.map_err(|err| VoiceError::Provider {
            provider: "core",
            message: format!("Failed to start durable task: {err}"),
        })?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(VoiceError::Provider {
                provider: "core",
                message: format!("Start durable task failed with status {status}"),
            });
        }

        response
            .json::<DurableTask>()
            .await
            .map_err(|err| VoiceError::Provider {
                provider: "core",
                message: format!("Invalid start task response: {err}"),
            })
    }

    pub async fn resume_durable_task(
        &self,
        context: &CallContext,
        task_id: Uuid,
    ) -> Result<DurableTask, VoiceError> {
        let endpoint = format!("{}/v1/durable-tasks/{task_id}/resume", self.base_url);
        let body = ContextRequest {
            host_context: HostContextRequest {
                host_user_id: context.external_identity.clone(),
                organization_external_key: None,
            },
        };
        let request = self
            .prepare_request(
                self.client.post(&endpoint),
                &context.external_identity,
                None,
            )?
            .json(&body);
        let response = request.send().await.map_err(|err| VoiceError::Provider {
            provider: "core",
            message: format!("Failed to resume durable task: {err}"),
        })?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(VoiceError::Provider {
                provider: "core",
                message: format!("Resume durable task failed with status {status}"),
            });
        }

        response
            .json::<DurableTask>()
            .await
            .map_err(|err| VoiceError::Provider {
                provider: "core",
                message: format!("Invalid resume task response: {err}"),
            })
    }

    pub async fn wait_durable_task(
        &self,
        context: &CallContext,
        task_id: Uuid,
        wait: WaitRequest,
    ) -> Result<DurableTask, VoiceError> {
        let endpoint = format!("{}/v1/durable-tasks/{task_id}/wait", self.base_url);
        let body = WaitRequestBody {
            host_context: HostContextRequest {
                host_user_id: context.external_identity.clone(),
                organization_external_key: None,
            },
            wait,
        };
        let request = self
            .prepare_request(
                self.client.post(&endpoint),
                &context.external_identity,
                None,
            )?
            .json(&body);
        let response = request.send().await.map_err(|err| VoiceError::Provider {
            provider: "core",
            message: format!("Failed to wait durable task: {err}"),
        })?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(VoiceError::Provider {
                provider: "core",
                message: format!("Wait durable task failed with status {status}"),
            });
        }

        response
            .json::<DurableTask>()
            .await
            .map_err(|err| VoiceError::Provider {
                provider: "core",
                message: format!("Invalid wait task response: {err}"),
            })
    }

    pub async fn cancel_durable_task(
        &self,
        context: &CallContext,
        task_id: Uuid,
    ) -> Result<DurableTask, VoiceError> {
        let endpoint = format!("{}/v1/durable-tasks/{task_id}/cancel", self.base_url);
        let body = ContextRequest {
            host_context: HostContextRequest {
                host_user_id: context.external_identity.clone(),
                organization_external_key: None,
            },
        };
        let request = self
            .prepare_request(
                self.client.post(&endpoint),
                &context.external_identity,
                None,
            )?
            .json(&body);
        let response = request.send().await.map_err(|err| VoiceError::Provider {
            provider: "core",
            message: format!("Failed to cancel durable task: {err}"),
        })?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(VoiceError::Provider {
                provider: "core",
                message: format!("Cancel durable task failed with status {status}"),
            });
        }

        response
            .json::<DurableTask>()
            .await
            .map_err(|err| VoiceError::Provider {
                provider: "core",
                message: format!("Invalid cancel task response: {err}"),
            })
    }

    pub async fn propose_action(
        &self,
        context: &CallContext,
        proposal: CreateProposalRequest,
    ) -> Result<Proposal, VoiceError> {
        let endpoint = format!("{}/v1/action-proposals", self.base_url);
        let body = ProposeRequestBody {
            host_context: HostContextRequest {
                host_user_id: context.external_identity.clone(),
                organization_external_key: None,
            },
            proposal,
        };
        let request = self
            .prepare_request(
                self.client.post(&endpoint),
                &context.external_identity,
                None,
            )?
            .json(&body);
        let response = request.send().await.map_err(|err| VoiceError::Provider {
            provider: "core",
            message: format!("Failed to propose action: {err}"),
        })?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(VoiceError::Provider {
                provider: "core",
                message: format!("Propose action failed with status {status}"),
            });
        }

        response
            .json::<Proposal>()
            .await
            .map_err(|err| VoiceError::Provider {
                provider: "core",
                message: format!("Invalid proposal response: {err}"),
            })
    }

    pub async fn approve_action_proposal(
        &self,
        context: &CallContext,
        proposal_id: Uuid,
        details: serde_json::Value,
    ) -> Result<Proposal, VoiceError> {
        let endpoint = format!(
            "{}/v1/action-proposals/{proposal_id}/approve",
            self.base_url
        );
        let body = ApproveRequestBody {
            host_context: HostContextRequest {
                host_user_id: context.external_identity.clone(),
                organization_external_key: None,
            },
            details,
        };
        let request = self
            .prepare_request(
                self.client.post(&endpoint),
                &context.external_identity,
                None,
            )?
            .json(&body);
        let response = request.send().await.map_err(|err| VoiceError::Provider {
            provider: "core",
            message: format!("Failed to approve action proposal: {err}"),
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            return Err(VoiceError::Provider {
                provider: "core",
                message: format!("Approve proposal failed ({status}): {body_text}"),
            });
        }

        response
            .json::<Proposal>()
            .await
            .map_err(|err| VoiceError::Provider {
                provider: "core",
                message: format!("Invalid approve response: {err}"),
            })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn report_reminder_delivery(
        &self,
        host_user_id: &str,
        reminder_id: Uuid,
        status: &str,
        channel: &str,
        destination: &str,
        provider_receipt_id: Option<&str>,
        failure_reason: Option<&str>,
    ) -> Result<(), VoiceError> {
        let endpoint = format!(
            "{}/v1/reminders/{reminder_id}/delivery-callback",
            self.base_url
        );
        let body = serde_json::json!({
            "host_context": HostContextRequest {
                host_user_id: host_user_id.to_string(),
                organization_external_key: None,
            },
            "status": status,
            "channel": channel,
            "destination": destination,
            "provider_receipt_id": provider_receipt_id,
            "failure_reason": failure_reason,
        });

        let request = self
            .prepare_request(self.client.post(&endpoint), host_user_id, None)?
            .json(&body);

        let response = request.send().await.map_err(|err| VoiceError::Provider {
            provider: "core",
            message: format!("Failed to report reminder delivery: {err}"),
        })?;

        if !response.status().is_success() {
            let s = response.status();
            let body_text = response.text().await.unwrap_or_default();
            return Err(VoiceError::Provider {
                provider: "core",
                message: format!("Report reminder delivery failed ({s}): {body_text}"),
            });
        }

        Ok(())
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
            .prepare_request(
                self.client.post(&self.endpoint),
                &context.external_identity,
                None,
            )?
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
            .prepare_request(
                self.client.post(&stream_endpoint),
                &context.external_identity,
                None,
            )?
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
                    response_bytes = body.len(),
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
            .prepare_request(
                self.client
                    .post(format!("{}/v1/conversations/speculate", self.base_url)),
                &context.external_identity,
                None,
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
        let payload = CompleteRequest {
            identity: IdentityPayload {
                channel: &context.channel,
                external_id: &context.external_identity,
            },
            external_conversation_id: &context.external_conversation_id,
        };
        let response = self
            .prepare_request(
                self.client.post(&endpoint),
                &context.external_identity,
                None,
            )?
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
