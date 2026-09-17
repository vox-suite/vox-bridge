use crate::voice::{
    context::CallContext,
    provider::{AgentProvider, TextStream, VoiceError},
};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub struct CoreAgentClient {
    client: reqwest::Client,
    base_url: String,
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
        let trimmed = base_url.trim_end_matches('/').to_owned();
        Ok(Self {
            client,
            endpoint: format!("{trimmed}/v1/conversations/respond"),
            base_url: trimmed,
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

    async fn respond_stream(
        &self,
        context: &CallContext,
        transcript: &str,
    ) -> Result<TextStream, VoiceError> {
        let stream_endpoint = format!("{}/v1/conversations/respond/stream", self.base_url);
        let request = RespondRequest {
            identity: Identity {
                channel: &context.channel,
                external_id: &context.external_identity,
            },
            external_conversation_id: &context.external_conversation_id,
            text: transcript,
            initiation_context: context.initiation_context.as_deref(),
        };

        let send_result = self
            .client
            .post(&stream_endpoint)
            .bearer_auth(&self.service_token)
            .header("Accept", "text/event-stream")
            .json(&request)
            .send()
            .await;

        match send_result {
            Ok(response) if response.status().is_success() => {
                let stream = response.bytes_stream();
                Ok(Box::pin(parse_sse_stream(stream)))
            }
            _ => {
                // Fallback to standard unary endpoint if streaming is unsupported by core
                let text = self.respond(context, transcript).await?;
                Ok(Box::pin(stream::once(async move { Ok(text) })))
            }
        }
    }

    async fn complete(&self, context: &CallContext) -> Result<(), VoiceError> {
        let endpoint = format!("{}/v1/conversations/complete", self.base_url);
        let response = self
            .client
            .post(&endpoint)
            .bearer_auth(&self.service_token)
            .json(&serde_json::json!({
                "identity": {
                    "channel": &context.channel,
                    "external_id": &context.external_identity,
                },
                "external_conversation_id": &context.external_conversation_id,
            }))
            .send()
            .await
            .map_err(|_| provider_error("Core complete request failed"))?;
        if !response.status().is_success() {
            return Err(provider_error("Core returned an error on complete"));
        }
        Ok(())
    }
}

fn parse_sse_stream<S>(bytes_stream: S) -> impl Stream<Item = Result<String, VoiceError>>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    stream::unfold(
        (bytes_stream, String::new(), false),
        |(mut stream, mut buffer, mut done)| async move {
            if done {
                return None;
            }

            loop {
                if let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim().to_string();
                    buffer = buffer[newline_pos + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            done = true;
                            return None;
                        }

                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                            if let Some(delta) = val.get("delta").and_then(|d| d.as_str()) {
                                if !delta.is_empty() {
                                    return Some((Ok(delta.to_string()), (stream, buffer, done)));
                                }
                            }
                            if let Some(text) = val.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    return Some((Ok(text.to_string()), (stream, buffer, done)));
                                }
                            }
                        } else if !data.is_empty() {
                            return Some((Ok(data.to_string()), (stream, buffer, done)));
                        }
                    }
                }

                match stream.next().await {
                    Some(Ok(bytes)) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                    }
                    Some(Err(err)) => {
                        return Some((
                            Err(VoiceError::Provider {
                                provider: "vox-core",
                                message: format!("streaming error: {err}"),
                            }),
                            (stream, buffer, true),
                        ));
                    }
                    None => {
                        if !buffer.trim().is_empty() {
                            let line = buffer.trim().to_string();
                            buffer.clear();
                            if let Some(data) = line.strip_prefix("data:") {
                                let data = data.trim();
                                if data != "[DONE]" && !data.is_empty() {
                                    return Some((Ok(data.to_string()), (stream, buffer, true)));
                                }
                            }
                        }
                        return None;
                    }
                }
            }
        },
    )
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
