use crate::voice::{
    context::CallContext,
    provider::{AgentEvent, AgentEventStream, AgentProvider, TextStream, VoiceError},
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
    tts_provider: Option<String>,
}

#[derive(Serialize)]
struct RespondRequest<'a> {
    identity: Identity<'a>,
    external_conversation_id: &'a str,
    text: &'a str,
    initiation_context: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice_signature: Option<&'a str>,
    turn_id: Option<&'a str>,
    revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tts_provider: Option<&'a str>,
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
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|_| configuration("Core HTTP client creation failed"))?;
        let trimmed = base_url.trim_end_matches('/').to_owned();
        Ok(Self {
            client,
            endpoint: format!("{trimmed}/v1/conversations/respond"),
            base_url: trimmed,
            service_token,
            tts_provider: None,
        })
    }

    pub fn with_tts_provider(mut self, provider: impl Into<String>) -> Self {
        self.tts_provider = Some(provider.into());
        self
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
                voice_signature: context.voice_signature.as_deref(),
                turn_id: context.turn_id.as_deref(),
                revision: context.revision,
                tts_provider: context
                    .tts_provider
                    .as_deref()
                    .or(self.tts_provider.as_deref()),
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

    async fn respond_events(
        &self,
        context: &CallContext,
        transcript: &str,
    ) -> Result<AgentEventStream, VoiceError> {
        let stream_endpoint = format!("{}/v1/conversations/respond/stream", self.base_url);
        let request = RespondRequest {
            identity: Identity {
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

        let start_time = std::time::Instant::now();
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
                tracing::info!(
                    conversation_id = %context.external_conversation_id,
                    endpoint = %stream_endpoint,
                    connect_time_ms = start_time.elapsed().as_millis(),
                    "CoreAgentClient: Stream connection established"
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
                    "CoreAgentClient: Stream unavailable, falling back to unary respond"
                );
                let text = self.respond(context, transcript).await?;
                Ok(Box::pin(stream::once(
                    async move { Ok(AgentEvent::Text(text)) },
                )))
            }
            _ => Err(provider_error("Core stream request failed")),
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
                        Ok(AgentEvent::Text(text)) => Some(Ok(text)),
                        Ok(AgentEvent::LookupPending) => None,
                        Err(err) => Some(Err(err)),
                    }
                }),
        ))
    }

    async fn speculate(&self, context: &CallContext, transcript: &str) -> Result<(), VoiceError> {
        let response = self
            .client
            .post(format!("{}/v1/conversations/speculate", self.base_url))
            .bearer_auth(&self.service_token)
            .timeout(Duration::from_secs(6))
            .json(&serde_json::json!({
                "identity": {"channel": context.channel, "external_id": context.external_identity},
                "external_conversation_id": context.external_conversation_id,
                "text": transcript, "turn_id": context.turn_id, "revision": context.revision,
                "tts_provider": context.tts_provider.as_deref().or(self.tts_provider.as_deref())
            }))
            .send()
            .await
            .map_err(|_| provider_error("Core speculation failed"))?;
        if !response.status().is_success() {
            return Err(provider_error("Core speculation unavailable"));
        }
        Ok(())
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

fn parse_sse_stream<S>(bytes_stream: S) -> impl Stream<Item = Result<AgentEvent, VoiceError>>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    stream::unfold(
        (bytes_stream, Vec::<u8>::new(), String::new(), false),
        |(mut source, mut buffer, mut event, mut ended)| async move {
            loop {
                let newline = buffer.iter().position(|byte| *byte == b'\n');
                if newline.is_some() || (ended && !buffer.is_empty()) {
                    let length = newline.map(|n| n + 1).unwrap_or(buffer.len());
                    let bytes: Vec<u8> = buffer.drain(..length).collect();
                    let line = match std::str::from_utf8(&bytes) {
                        Ok(line) => line.trim(),
                        Err(_) => {
                            return Some((
                                Err(provider_error("Core returned invalid UTF-8")),
                                (source, Vec::new(), event, true),
                            ));
                        }
                    };
                    if line.is_empty() {
                        event.clear();
                        continue;
                    }
                    if let Some(value) = line.strip_prefix("event:") {
                        event = value.trim().into();
                        continue;
                    }
                    if let Some(data) = line.strip_prefix("data:") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return None;
                        }
                        if event == "lookup_pending" {
                            event.clear();
                            return Some((
                                Ok(AgentEvent::LookupPending),
                                (source, buffer, event, ended),
                            ));
                        }
                        let value = match serde_json::from_str::<serde_json::Value>(data) {
                            Ok(value) => value,
                            Err(_) => {
                                return Some((
                                    Err(provider_error("Core returned invalid SSE data")),
                                    (source, Vec::new(), event, true),
                                ));
                            }
                        };
                        if value.get("error").is_some() || event == "error" {
                            return Some((
                                Err(provider_error("Core stream returned an error")),
                                (source, Vec::new(), event, true),
                            ));
                        }
                        if let Some(text) = value
                            .get("delta")
                            .or_else(|| value.get("text"))
                            .and_then(|value| value.as_str())
                            .filter(|text| !text.is_empty())
                        {
                            return Some((
                                Ok(AgentEvent::Text(text.into())),
                                (source, buffer, event, ended),
                            ));
                        }
                    }
                    continue;
                }
                if ended {
                    return None;
                }
                match source.next().await {
                    Some(Ok(bytes)) => {
                        buffer.extend_from_slice(&bytes);
                        if buffer.len() > 1024 * 1024 {
                            return Some((
                                Err(provider_error("Core SSE frame too large")),
                                (source, Vec::new(), event, true),
                            ));
                        }
                    }
                    Some(Err(_)) => {
                        return Some((
                            Err(provider_error("Core stream disconnected")),
                            (source, Vec::new(), event, true),
                        ));
                    }
                    None => ended = true,
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

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[tokio::test]
    async fn handles_fragmented_unicode_pending_and_unterminated_last_frame() {
        let data = "event: lookup_pending\ndata: {}\n\ndata: {\"delta\":\"नमस्ते\"}";
        let bytes = data
            .as_bytes()
            .iter()
            .map(|byte| Ok(Bytes::from(vec![*byte])))
            .collect::<Vec<_>>();
        let events = parse_sse_stream(stream::iter(bytes))
            .collect::<Vec<_>>()
            .await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].as_ref().unwrap(), &AgentEvent::LookupPending);
        assert_eq!(
            events[1].as_ref().unwrap(),
            &AgentEvent::Text("नमस्ते".into())
        );
    }

    #[tokio::test]
    async fn stream_error_is_not_spoken_as_text() {
        let events = parse_sse_stream(stream::iter(vec![Ok(Bytes::from_static(
            b"event: error\ndata: {\"error\":\"unavailable\"}\n\n",
        ))]))
        .collect::<Vec<_>>()
        .await;
        assert_eq!(events.len(), 1);
        assert!(events[0].is_err());
    }
}
