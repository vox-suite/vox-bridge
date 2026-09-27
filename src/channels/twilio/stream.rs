/**
* this file code contains twilio websocket audio stream handling
*/
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;

use crate::channels::context::CallContext;
use crate::channels::twilio::protocol::{
    InboundStreamMessage, OutboundClearMessage, OutboundMark, OutboundMarkMessage,
    OutboundMediaMessage, OutboundPayload, TwilioStartPayload,
};
use crate::channels::twilio::signature::validate_twilio_signature;
use crate::channels::twilio::webhook::{TwilioState, VOICE_STREAM_URL};
use crate::state::AppState;
use crate::voice::provider::VoiceError;
use crate::voice::session::{
    CallCommand, CallEvent, VoiceSessionHandle, shutdown_playback_session, spawn_playback_session,
};

pub fn parse_inbound(raw: &str) -> Result<InboundStreamMessage, VoiceError> {
    serde_json::from_str(raw).map_err(|_| VoiceError::Protocol("invalid Twilio event".into()))
}

pub fn serialize_command(stream_sid: &str, command: &CallCommand) -> Result<String, VoiceError> {
    let result = match command {
        CallCommand::Media { bytes, .. } => serde_json::to_string(&OutboundMediaMessage {
            event: "media",
            stream_sid,
            media: OutboundPayload {
                payload: &STANDARD.encode(bytes),
            },
        }),
        CallCommand::Mark { name, .. } => serde_json::to_string(&OutboundMarkMessage {
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
                InboundStreamMessage::Media { .. } | InboundStreamMessage::Mark { .. } => {
                    return Err(VoiceError::Protocol(
                        "Twilio media arrived before start".into(),
                    ));
                }
                InboundStreamMessage::Unknown => {}
            },
            Some(Ok(Message::Close(_))) | None => return Ok(()),
            Some(Ok(_)) => {}
            Some(Err(_)) => {
                return Err(VoiceError::Protocol("Twilio socket receive failed".into()));
            }
        }
    };
    if !state.twilio.contains_key(&start.call_sid) {
        restore_outbound_state(&state, &start)?;
    }
    validate_start(&state.twilio, &start)?;
    let call_sid = start.call_sid.clone();
    let stream_sid = start.stream_sid;
    let accepted_call = state
        .twilio
        .get(&call_sid)
        .map(|entry| entry.clone())
        .ok_or_else(|| VoiceError::Protocol("Twilio stream has no accepted call".into()))?;
    tracing::info!(%stream_sid, %call_sid, "Twilio stream started");
    let profile = state.voice.resolver.resolve();
    let context = CallContext {
        channel: "phone".into(),
        external_identity: accepted_call.from,
        external_conversation_id: accepted_call.external_conversation_id.clone(),
        initiation_context: accepted_call.opening_instruction.clone(),
        turn_id: None,
        revision: None,
        tts_provider: Some(profile.tts.provider.clone()),
        filler: None,
    };
    let providers = state.voice.providers.providers_for(&profile)?;
    let VoiceSessionHandle {
        playback,
        input: input_tx,
        output: mut output_rx,
        task: mut voice_task,
    } = spawn_playback_session(providers.clone(), context.clone());
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
                            InboundStreamMessage::Mark { stream_sid: incoming_sid, mark } => {
                                if incoming_sid != stream_sid {
                                    break Err(VoiceError::Protocol("Twilio stream identifier changed".into()));
                                }
                                input_tx.try_send(CallEvent::PlaybackFinished(mark.name))
                                    .map_err(|_| VoiceError::Protocol("Twilio playback buffer unavailable".into()))?;
                            }
                            InboundStreamMessage::Connected { .. } | InboundStreamMessage::Start { .. } => {
                                break Err(VoiceError::Protocol("unexpected Twilio stream event".into()));
                            }
                            InboundStreamMessage::Unknown => {}
                        },
                        Some(Ok(Message::Close(_))) | None => break Ok(()),
                        Some(Ok(_)) => {}
                        Some(Err(_)) => break Err(VoiceError::Protocol("Twilio socket receive failed".into())),
                    }
                }
                command = output_rx.recv() => {
                    match command {
                        Some(CallCommand::Media { bytes, generation }) => {
                            if playback.accepts(generation) {
                                let message = serialize_command(&stream_sid, &CallCommand::Media { bytes, generation })?;
                                sender.send(Message::Text(message.into())).await
                                    .map_err(|_| VoiceError::Protocol("Twilio socket send failed".into()))?;
                            } else {
                                tracing::debug!(generation, "Discarding stale media for invalidated playback generation");
                            }
                        }
                        Some(CallCommand::Mark { name, generation }) => {
                            if playback.accepts(generation) {
                                let message = serialize_command(&stream_sid, &CallCommand::Mark { name, generation })?;
                                sender.send(Message::Text(message.into())).await
                                    .map_err(|_| VoiceError::Protocol("Twilio socket send failed".into()))?;
                            } else {
                                tracing::debug!(generation, "Discarding stale mark for invalidated playback generation");
                            }
                        }
                        Some(CallCommand::Clear) => {
                            let message = serialize_command(&stream_sid, &CallCommand::Clear)?;
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
    shutdown_playback_session(voice_task, &providers, &context).await;
    state.twilio.remove(&call_sid);
    tracing::info!(%stream_sid, %call_sid, "Twilio media stream closed");
    result
}

fn restore_outbound_state(state: &AppState, start: &TwilioStartPayload) -> Result<(), VoiceError> {
    let value = |name: &str| {
        start
            .custom_parameters
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| VoiceError::Protocol("Twilio stream has no accepted call".into()))
    };
    state.twilio.insert(
        start.call_sid.clone(),
        TwilioState {
            call_sid: start.call_sid.clone(),
            account_sid: state.twilio_account_sid.to_string(),
            from: value("external_identity")?,
            to: state.twilio_from_number.to_string(),
            call_status: Some("in_progress".into()),
            opening_instruction: Some(value("opening_instruction")?),
            action_id: Some(value("action_id")?),
            external_conversation_id: value("external_conversation_id")?,
        },
    );
    Ok(())
}

fn validate_start(
    calls: &DashMap<String, TwilioState>,
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
