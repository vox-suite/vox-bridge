use crate::voice::{
    context::CallContext,
    provider::{AgentEvent, AgentEventStream, AgentProvider, TextStream, VoiceError},
};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::Duration;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub struct CoreAgentClient {
    client: reqwest::Client,
    base_url: String,
    endpoint: String,
    host_credential_id: Uuid,
    host_audience: String,
    host_secret: String,
    tts_provider: Option<String>,
}

#[derive(Serialize)]
struct HostContext<'a> {
    host_user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization_external_key: Option<&'a str>,
}

#[derive(Deserialize)]
struct RespondResponse {
    text: String,
}

impl CoreAgentClient {
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
            .map_err(|_| configuration("Core HTTP client creation failed"))?;
        let trimmed = base_url.trim_end_matches('/').to_owned();
        let host_credential_id = Uuid::parse_str(host_credential_id.trim())
            .map_err(|_| configuration("VOX_CORE_HOST_CREDENTIAL_ID is invalid"))?;
        if host_audience.trim().is_empty() || host_secret.trim().is_empty() {
            return Err(configuration(
                "Core host credential configuration is missing",
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

    fn host_context(&self, context: &CallContext) -> HostContext<'_> {
        let normalized_phone = crate::voice::context::normalized_e164(&context.external_identity)
            .unwrap_or_else(|| context.external_identity.clone());
        HostContext {
            // Channel-prefixed contexts intentionally prevent implicit cross-channel linking.
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
            .map_err(|_| configuration("system clock is before Unix epoch"))?
            .as_secs() as i64;
        let nonce = Uuid::new_v4();
        let host_context = self.host_context(context);
        let canonical = canonical_assertion(
            self.host_credential_id,
            &self.host_audience,
            issued_at,
            nonce,
            &host_context.host_user_id,
        );
        let mut signer = HmacSha256::new_from_slice(self.host_secret.as_bytes())
            .map_err(|_| configuration("Core host credential is invalid"))?;
        signer.update(canonical.as_bytes());
        let signature = hex::encode(signer.finalize().into_bytes());
        Ok(request
            .header("X-Vox-Host-Credential", self.host_credential_id.to_string())
            .header("X-Vox-Host-Audience", &self.host_audience)
            .header("X-Vox-Host-Timestamp", issued_at.to_string())
            .header("X-Vox-Host-Nonce", nonce.to_string())
            .header("X-Vox-Host-Signature", signature)
            // Existing Core host trust verifies this over TLS and never logs it.
            .header("X-Vox-Host-Secret", &self.host_secret))
    }
}

#[async_trait]
impl AgentProvider for CoreAgentClient {
    async fn respond(&self, context: &CallContext, transcript: &str) -> Result<String, VoiceError> {
        let host_context = self.host_context(context);
        let response = self
            .signed_request(self.client.post(&self.endpoint), context)?
            .json(&serde_json::json!({
                "host_context": host_context,
                "identity": {"channel": context.channel, "external_id": context.external_identity},
                "external_conversation_id": context.external_conversation_id,
                "text": transcript,
                "initiation_context": context.initiation_context,
                "voice_signature": context.voice_signature,
                "turn_id": context.turn_id,
                "revision": context.revision,
                "tts_provider": context.tts_provider.as_deref().or(self.tts_provider.as_deref()),
            }))
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
        let start_time = std::time::Instant::now();
        let host_context = self.host_context(context);
        let send_result = self
            .signed_request(self.client.post(&stream_endpoint), context)?
            .header("Accept", "text/event-stream")
            .json(&serde_json::json!({
                "host_context": host_context,
                "identity": {"channel": context.channel, "external_id": context.external_identity},
                "external_conversation_id": context.external_conversation_id,
                "text": transcript,
                "initiation_context": context.initiation_context,
                "voice_signature": context.voice_signature,
                "turn_id": context.turn_id,
                "revision": context.revision,
                "tts_provider": context.tts_provider.as_deref().or(self.tts_provider.as_deref()),
            }))
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
        let host_context = self.host_context(context);
        let response = self
            .signed_request(
                self.client
                    .post(format!("{}/v1/conversations/speculate", self.base_url)),
                context,
            )?
            .timeout(Duration::from_secs(6))
            .json(&serde_json::json!({
                "host_context": host_context,
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
        let host_context = self.host_context(context);
        let response = self
            .signed_request(self.client.post(&endpoint), context)?
            .json(&serde_json::json!({
                "host_context": host_context,
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

fn canonical_assertion(
    credential_id: Uuid,
    audience: &str,
    issued_at: i64,
    nonce: Uuid,
    host_user_id: &str,
) -> String {
    let mut canonical = String::from("vox-host-assertion-v1");
    for field in [
        credential_id.to_string(),
        audience.to_owned(),
        issued_at.to_string(),
        nonce.to_string(),
        host_user_id.to_owned(),
        String::new(),
    ] {
        canonical.push('|');
        canonical.push_str(&field.len().to_string());
        canonical.push(':');
        canonical.push_str(&field);
    }
    canonical
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
