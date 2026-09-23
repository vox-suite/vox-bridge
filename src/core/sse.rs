/**
* this file code contains server sent events parser for core streaming
*/
use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};

use crate::core::ConversationEvent;
use crate::voice::provider::VoiceError;

pub fn parse_sse_stream<S>(
    bytes_stream: S,
) -> impl Stream<Item = Result<ConversationEvent, VoiceError>>
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
                                Err(VoiceError::Provider {
                                    provider: "core",
                                    message: "Core returned invalid UTF-8".into(),
                                }),
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
                                Ok(ConversationEvent::LookupPending),
                                (source, buffer, event, ended),
                            ));
                        }
                        let value = match serde_json::from_str::<serde_json::Value>(data) {
                            Ok(value) => value,
                            Err(_) => {
                                return Some((
                                    Err(VoiceError::Provider {
                                        provider: "core",
                                        message: "Core returned invalid SSE data".into(),
                                    }),
                                    (source, Vec::new(), event, true),
                                ));
                            }
                        };
                        if value.get("error").is_some() || event == "error" {
                            return Some((
                                Err(VoiceError::Provider {
                                    provider: "core",
                                    message: "Core stream returned an error".into(),
                                }),
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
                                Ok(ConversationEvent::Text(text.into())),
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
                                Err(VoiceError::Provider {
                                    provider: "core",
                                    message: "Core SSE frame too large".into(),
                                }),
                                (source, Vec::new(), event, true),
                            ));
                        }
                    }
                    Some(Err(_)) => {
                        return Some((
                            Err(VoiceError::Provider {
                                provider: "core",
                                message: "Core stream disconnected".into(),
                            }),
                            (source, Vec::new(), event, true),
                        ));
                    }
                    None => {
                        ended = true;
                    }
                }
            }
        },
    )
}
