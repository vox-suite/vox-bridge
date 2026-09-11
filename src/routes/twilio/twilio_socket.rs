use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use crate::{
    routes::twilio::twilio_post::{VOICE_STREAM_URL, validate_twilio_signature},
    voice::{
        provider::VoiceError,
        session::{CallCommand, CallEvent, run_voice_session},
    },
    AppState,
};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Deserialize, Debug)]
pub struct TwilioMediaPayload {
    pub payload: String,
}

#[derive(Deserialize, Debug)]
pub struct TwilioStartPayload {
    #[serde(rename = "streamSid")]
    pub stream_sid: String,
    #[serde(rename = "callSid")]
    pub call_sid: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event")]
pub enum InboundStreamMessage {
    #[serde(rename = "connected")]
    Connected { protocol: String, version: String },
    #[serde(rename = "start")]
    Start { start: TwilioStartPayload },
    #[serde(rename = "media")]
    Media {
        #[serde(rename = "streamSid")]
        stream_sid: String,
        media: TwilioMediaPayload,
    },
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "mark")]
    Mark,
}

#[derive(Debug, Serialize)]
pub struct OutboundMediaMessage {
    pub event: &'static str,
    #[serde(rename = "streamSid")]
    pub stream_sid: String,
    pub media: OutboundPayload,
}

#[derive(Debug, Serialize)]
pub struct OutboundPayload {
    pub payload: String,
}

#[derive(Debug, Serialize)]
struct OutboundMarkMessage<'a> {
    event: &'static str,
    #[serde(rename = "streamSid")]
    stream_sid: &'a str,
    mark: OutboundMark<'a>,
}

#[derive(Debug, Serialize)]
struct OutboundMark<'a> {
    name: &'a str,
}

#[derive(Debug, Serialize)]
struct OutboundClearMessage<'a> {
    event: &'static str,
    #[serde(rename = "streamSid")]
    stream_sid: &'a str,
}

fn parse_inbound(raw: &str) -> Result<InboundStreamMessage, VoiceError> {
    serde_json::from_str(raw).map_err(|_| VoiceError::Protocol("invalid Twilio event".into()))
}

fn serialize_command(stream_sid: &str, command: &CallCommand) -> Result<String, VoiceError> {
    let result = match command {
        CallCommand::Media(audio) => serde_json::to_string(&OutboundMediaMessage {
            event: "media",
            stream_sid: stream_sid.into(),
            media: OutboundPayload {
                payload: STANDARD.encode(audio),
            },
        }),
        CallCommand::Mark(name) => serde_json::to_string(&OutboundMarkMessage {
            event: "mark",
            stream_sid,
            mark: OutboundMark { name },
        }),
        CallCommand::Clear => serde_json::to_string(&OutboundClearMessage {
            event: "clear",
            stream_sid,
        }),
    };
    result.map_err(|_| VoiceError::Protocol("Twilio command serialization failed".into()))
}

fn validate_stream_signature(headers: &axum::http::HeaderMap, auth_token: &str) -> bool {
    headers
        .get("x-twilio-signature")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|signature| {
            validate_twilio_signature(auth_token, VOICE_STREAM_URL, &[], signature)
        })
}

pub async fn voice_stream_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if headers.get("x-twilio-signature").is_none() {
        return (StatusCode::UNAUTHORIZED, "Missing Twilio signature").into_response();
    }
    if !validate_stream_signature(&headers, state.twilio_auth_token.as_str()) {
        return (StatusCode::FORBIDDEN, "Invalid Twilio signature").into_response();
    }
    ws.on_upgrade(move |socket| handle_voice_socket(socket, state))
        .into_response()
}

async fn handle_voice_socket(socket: WebSocket, state: Arc<AppState>) {
    if let Err(error) = run_twilio_socket(socket, state).await {
        tracing::warn!(bridge_error = %error, "Twilio media stream failed");
    }
}

async fn run_twilio_socket(socket: WebSocket, state: Arc<AppState>) -> Result<(), VoiceError> {
    let (mut sender, mut receiver) = socket.split();
    tracing::info!("Twilio media stream connected");
    let start = loop {
        match receiver.next().await {
            Some(Ok(Message::Text(raw))) => match parse_inbound(raw.as_str())? {
                InboundStreamMessage::Connected { protocol, version } => {
                tracing::info!(protocol, version, "Twilio handshake accepted");
                }
                InboundStreamMessage::Start { start } => break start,
                InboundStreamMessage::Stop => return Ok(()),
                InboundStreamMessage::Media { .. } | InboundStreamMessage::Mark => {
                    return Err(VoiceError::Protocol(
                        "Twilio media arrived before start".into(),
                    ));
                }
            },
            Some(Ok(Message::Close(_))) | None => return Ok(()),
            Some(Ok(_)) => {}
            Some(Err(_)) => {
                return Err(VoiceError::Protocol("Twilio socket receive failed".into()));
            }
        }
    };
    validate_start(&state.twilio, &start)?;
    let call_sid = start.call_sid.clone();
    let stream_sid = start.stream_sid;
    tracing::info!(%stream_sid, %call_sid, "Twilio stream started");
    let profile = state.voice.resolver.resolve();
    let providers = state.voice.providers.providers_for(&profile)?;
    let (input_tx, input_rx) = mpsc::channel(64);
    let (output_tx, mut output_rx) = mpsc::channel(64);
    let mut voice_task = tokio::spawn(run_voice_session(providers, input_rx, output_tx));
    let result = async {
        loop {
            tokio::select! {
                message = receiver.next() => {
                    match message {
                        Some(Ok(Message::Text(raw))) => match parse_inbound(raw.as_str())? {
                            InboundStreamMessage::Media { stream_sid: incoming_sid, media } => {
                                if incoming_sid != stream_sid {
                                    break Err(VoiceError::Protocol("Twilio stream identifier changed".into()));
                                }
                                let audio = STANDARD.decode(media.payload)
                                    .map_err(|_| VoiceError::Protocol("invalid Twilio media payload".into()))?;
                                input_tx.try_send(CallEvent::Audio(audio.into()))
                                    .map_err(|_| VoiceError::Protocol("Twilio audio buffer unavailable".into()))?;
                            }
                            InboundStreamMessage::Stop => break Ok(()),
                            InboundStreamMessage::Mark => {}
                            InboundStreamMessage::Connected { .. } | InboundStreamMessage::Start { .. } => {
                                break Err(VoiceError::Protocol("unexpected Twilio stream event".into()));
                            }
                        },
                        Some(Ok(Message::Close(_))) | None => break Ok(()),
                        Some(Ok(_)) => {}
                        Some(Err(_)) => break Err(VoiceError::Protocol("Twilio socket receive failed".into())),
                    }
                }
                command = output_rx.recv() => {
                    match command {
                        Some(command) => {
                            let message = serialize_command(&stream_sid, &command)?;
                            sender.send(Message::Text(message.into())).await
                                .map_err(|_| VoiceError::Protocol("Twilio socket send failed".into()))?;
                        }
                        None => break Ok(()),
                    }
                }
                voice_result = &mut voice_task => {
                    break voice_result
                        .map_err(|_| VoiceError::Protocol("voice session task failed".into()))?;
                }
            }
        }
    }
    .await;
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        input_tx.send(CallEvent::Stop),
    )
    .await;
    if !voice_task.is_finished() {
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut voice_task)
            .await
            .is_err()
        {
            voice_task.abort();
        }
    }
    state.twilio.remove(&call_sid);
    tracing::info!(%stream_sid, %call_sid, "Twilio media stream closed");
    result
}

fn validate_start(
    calls: &DashMap<String, crate::routes::twilio::twilio_post::TwilioState>,
    start: &TwilioStartPayload,
) -> Result<(), VoiceError> {
    if calls.contains_key(&start.call_sid) {
        Ok(())
    } else {
        Err(VoiceError::Protocol(
            "Twilio stream has no accepted call".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::session::CallCommand;
    use hmac::{Hmac, Mac};
    use sha1::Sha1;

    #[test]
    fn parses_twilio_start_and_media_events() {
        let start = parse_inbound(
            r#"{"event":"start","streamSid":"MZ123","start":{"streamSid":"MZ123","callSid":"CA123"}}"#,
        )
        .unwrap();
        let media = parse_inbound(
            r#"{"event":"media","streamSid":"MZ123","media":{"payload":"AQI=","track":"inbound","chunk":"1","timestamp":"20"}}"#,
        )
        .unwrap();

        assert!(matches!(start, InboundStreamMessage::Start { start } if start.stream_sid == "MZ123" && start.call_sid == "CA123"));
        assert!(matches!(media, InboundStreamMessage::Media { stream_sid, media } if stream_sid == "MZ123" && media.payload == "AQI="));
    }

    #[test]
    fn serializes_media_mark_and_clear_commands() {
        assert_eq!(
            serialize_command(
                "MZ123",
                &CallCommand::Media(bytes::Bytes::from_static(&[1, 2]))
            )
            .unwrap(),
            r#"{"event":"media","streamSid":"MZ123","media":{"payload":"AQI="}}"#
        );
        assert_eq!(
            serialize_command("MZ123", &CallCommand::Mark("response-1".into())).unwrap(),
            r#"{"event":"mark","streamSid":"MZ123","mark":{"name":"response-1"}}"#
        );
        assert_eq!(
            serialize_command("MZ123", &CallCommand::Clear).unwrap(),
            r#"{"event":"clear","streamSid":"MZ123"}"#
        );
    }

    #[test]
    fn validates_stream_signatures() {
        let token = "test-token";
        let mut mac = Hmac::<Sha1>::new_from_slice(token.as_bytes()).unwrap();
        mac.update(VOICE_STREAM_URL.as_bytes());
        let signature = STANDARD.encode(mac.finalize().into_bytes());
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-twilio-signature", signature.parse().unwrap());

        assert!(validate_stream_signature(&headers, token));
        assert!(!validate_stream_signature(
            &axum::http::HeaderMap::new(),
            token
        ));
        headers.insert("x-twilio-signature", "aW52YWxpZA==".parse().unwrap());
        assert!(!validate_stream_signature(&headers, token));
    }

    #[test]
    fn requires_an_accepted_call_before_binding_a_stream() {
        let calls = dashmap::DashMap::new();
        let start = TwilioStartPayload {
            stream_sid: "MZ123".into(),
            call_sid: "CA123".into(),
        };
        assert!(validate_start(&calls, &start).is_err());

        calls.insert(
            "CA123".into(),
            crate::routes::twilio::twilio_post::TwilioState {
                call_sid: "CA123".into(),
                account_sid: "AC123".into(),
                from: "+14155550100".into(),
                to: "+14155550101".into(),
                call_status: None,
            },
        );
        assert!(validate_start(&calls, &start).is_ok());
    }
}
