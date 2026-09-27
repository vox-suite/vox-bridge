/**
* this file code contains desktop websocket stream handler
*/
use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket, rejection::WebSocketUpgradeRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;

use crate::channels::context::CallContext;
use crate::channels::desktop::protocol::{
    DesktopInboundText, DesktopOutboundText, parse_inbound_text, serialize_outbound_text,
};
use crate::channels::desktop::session::DesktopSessionState;
use crate::state::AppState;
use crate::voice::session::{
    CallCommand, CallEvent, VoiceSessionHandle, shutdown_playback_session, spawn_playback_session,
};

#[derive(Deserialize)]
pub struct DesktopStreamParams {
    pub ticket: String,
}

pub async fn desktop_stream_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DesktopStreamParams>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Some((_, session)) = state.desktop_sessions.remove(&params.ticket) else {
        tracing::warn!("rejected desktop websocket stream with unknown or replayed ticket");
        return (StatusCode::UNAUTHORIZED, "Invalid or expired ticket").into_response();
    };

    if session.expires_at < Instant::now() {
        tracing::warn!("rejected desktop websocket stream with expired ticket");
        return (StatusCode::UNAUTHORIZED, "Ticket expired").into_response();
    }

    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };

    ws.on_upgrade(move |socket| handle_desktop_socket(socket, state, session))
}

async fn handle_desktop_socket(
    socket: WebSocket,
    state: Arc<AppState>,
    session: DesktopSessionState,
) {
    let (mut sender, mut receiver) = socket.split();

    let connected = DesktopOutboundText::Connected {
        session_id: session.external_conversation_id.clone(),
        sample_rate: 8000,
        format: "pcm_mulaw".into(),
    };
    if let Ok(msg) = serialize_outbound_text(&connected)
        && sender.send(Message::Text(msg.into())).await.is_err()
    {
        return;
    }

    let start_received = loop {
        match receiver.next().await {
            Some(Ok(Message::Text(raw))) => match parse_inbound_text(&raw) {
                Ok(DesktopInboundText::Start { .. }) => break true,
                Ok(DesktopInboundText::Stop) => return,
                _ => continue,
            },
            Some(Ok(Message::Close(_))) | None => return,
            Some(Ok(_)) => continue,
            Some(Err(e)) => {
                tracing::warn!(error = %e, "Desktop WebSocket handshake failed");
                return;
            }
        }
    };

    if !start_received {
        return;
    }

    let profile = state.voice.resolver.resolve();
    let context = CallContext {
        channel: "desktop".into(),
        external_identity: session.host_user_id,
        external_conversation_id: session.external_conversation_id,
        initiation_context: session.opening_instruction,
        turn_id: None,
        revision: None,
        tts_provider: Some(profile.tts.provider.clone()),
        filler: None,
    };

    let providers = match state.voice.providers.providers_for(&profile) {
        Ok(p) => p,
        Err(err) => {
            tracing::error!(error = %err, "Failed to resolve voice providers for desktop session");
            return;
        }
    };

    let VoiceSessionHandle {
        playback,
        input: input_tx,
        output: mut output_rx,
        task: mut voice_task,
    } = spawn_playback_session(providers.clone(), context.clone());

    loop {
        tokio::select! {
            inbound = receiver.next() => {
                match inbound {
                    Some(Ok(Message::Binary(bytes))) => {
                        if !bytes.is_empty()
                            && input_tx.send(CallEvent::Audio(bytes)).await.is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(raw))) => {
                        match parse_inbound_text(&raw) {
                            Ok(DesktopInboundText::Mark { name }) => {
                                if input_tx.send(CallEvent::PlaybackFinished(name)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(DesktopInboundText::Stop) => {
                                let _ = input_tx.send(CallEvent::Stop).await;
                                break;
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        let _ = input_tx.send(CallEvent::Stop).await;
                        break;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = sender.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(err)) => {
                        tracing::warn!(error = %err, "Desktop WebSocket read error");
                        let _ = input_tx.send(CallEvent::Stop).await;
                        break;
                    }
                }
            }
            command = output_rx.recv() => {
                match command {
                    // Audio and marks from a reply the user already interrupted are
                    // dropped, exactly as on the Twilio path.
                    Some(CallCommand::Media { bytes, generation }) => {
                        if playback.accepts(generation)
                            && sender.send(Message::Binary(bytes)).await.is_err()
                        {
                            break;
                        }
                    }
                    Some(CallCommand::Mark { name, generation }) => {
                        if !playback.accepts(generation) {
                            continue;
                        }
                        if let Ok(text) = serialize_outbound_text(&DesktopOutboundText::Mark { name })
                            && sender.send(Message::Text(text.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    Some(CallCommand::Clear) => {
                        if let Ok(text) = serialize_outbound_text(&DesktopOutboundText::Clear)
                            && sender.send(Message::Text(text.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    None => {
                        break;
                    }
                }
            }
            session_result = &mut voice_task => {
                match session_result {
                    Ok(Ok(())) => tracing::info!("Desktop voice session completed normally"),
                    Ok(Err(err)) => tracing::warn!(error = %err, "Desktop voice session ended with error"),
                    Err(join_err) => tracing::error!(error = %join_err, "Desktop voice session task panicked"),
                }
                break;
            }
        }
    }

    // Close out the conversation (summaries, completion) like the Twilio path.
    shutdown_playback_session(voice_task, &providers, &context).await;
}
