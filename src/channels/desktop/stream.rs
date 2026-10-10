use super::protocol::{Control, parse_control};
use crate::voice::session::input::admit_input;
use crate::{
    channels::context::CallContext,
    core::desktop::DesktopConversationClient,
    state::AppState,
    voice::{
        provider::VoiceError,
        session::{CallCommand, CallEvent, shutdown_playback_session, spawn_playback_session},
    },
};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::time::Instant;
use std::{sync::Arc, time::Duration};

pub async fn handler(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.max_message_size(8192)
        .max_frame_size(8192)
        .on_upgrade(move |socket| async move {
            if let Err(error) = run(socket, state).await {
                tracing::warn!(error_kind = error.kind(), "Desktop voice transport ended");
            }
        })
}
async fn run(socket: WebSocket, state: Arc<AppState>) -> Result<(), VoiceError> {
    let (mut sender, mut receiver) = socket.split();
    let first = tokio::time::timeout(Duration::from_secs(5), receiver.next())
        .await
        .map_err(|_| VoiceError::Timeout("desktop_start"))?;
    let ticket = match first {
        Some(Ok(Message::Text(raw))) => match parse_control(&raw)? {
            Control::Start { ticket } => ticket,
            _ => return Err(VoiceError::Protocol("start_required".into())),
        },
        _ => return Err(VoiceError::Protocol("start_required".into())),
    };
    let (client, binding) =
        DesktopConversationClient::redeem(&state.core_url, &state.service_token, &ticket).await?;
    let profile = state.voice.resolver.resolve();
    let mut providers = state.voice.providers.providers_for(&profile)?;
    providers.agent = Arc::new(client);
    let context = CallContext {
        channel: "voice".into(),
        external_identity: binding.user_id,
        external_conversation_id: format!("desktop:{}", binding.session_id),
        initiation_context: Some(
            "Start the live desktop conversation with a brief greeting.".into(),
        ),
        turn_id: None,
        revision: None,
        tts_provider: Some(profile.tts.provider),
        filler: None,
    };
    let mut handle = spawn_playback_session(providers.clone(), context.clone());
    let connected = json!({"type":"connected","session_id":binding.session_id,"device_id":binding.device_id,"audio_format":"mulaw","sample_rate":8000,"input_mode":"stream","speech_detector":"earshot"});
    let result=async {
        send_frame(&mut sender,Message::Text(connected.to_string().into())).await.map_err(|_|VoiceError::Protocol("connection_closed".into()))?;
        let mut heartbeat=tokio::time::interval(Duration::from_secs(20));
        let mut pending_input: Option<(CallEvent, Instant)> = None;
        let mut transport = crate::voice::transport_metrics::TransportMetrics::default();
        loop {
            let deadline = pending_input.as_ref().map(|(_, at)| tokio::time::Instant::from_std(*at) + Duration::from_secs(2));
            tokio::select! {
                message=receiver.next(), if pending_input.is_none()=>match message {
                    Some(Ok(Message::Binary(bytes)))=>{
                        if bytes.len() > 800 { return Err(VoiceError::Protocol("audio_frame_too_large".into())); }
                        pending_input = admit_input(&handle.input, CallEvent::Audio(bytes))?;
                    },
                    Some(Ok(Message::Text(raw)))=>match parse_control(&raw)? {
                        Control::PlaybackFinished {name}=>{transport.acknowledged(&name);pending_input=admit_input(&handle.input,CallEvent::PlaybackFinished(name))?;},
                        Control::Text {text}=>pending_input=admit_input(&handle.input,CallEvent::Text(text))?,
                        Control::Stop=>break,
                        Control::Start {..}=>return Err(VoiceError::Protocol("duplicate_start".into()))
                    },
                    Some(Ok(Message::Close(_)))|None|Some(Err(_))=>break,
                    _=>{}
                },
                permit=handle.input.reserve(), if pending_input.is_some()=>{
                    let permit=permit.map_err(|_|VoiceError::Protocol("input_receiver_closed".into()))?;
                    if let Some((event,at))=pending_input.take(){
                        tracing::info!(wait_ms=at.elapsed().as_millis() as u64,"VOICE_INPUT_BACKPRESSURE_RECOVERED");
                        permit.send(event);
                    }
                },
                _=async { match deadline { Some(at)=>tokio::time::sleep_until(at).await, None=>std::future::pending::<()>().await } }=>{
                    tracing::warn!(timeout_ms=2000,"VOICE_INPUT_BACKPRESSURE_TIMEOUT");
                    return Err(VoiceError::Protocol("audio_backpressure_timeout".into()));
                },
                command=handle.output.recv()=>{
                    let Some(command)=command else {break};
                    let message=match command {
                        CallCommand::Media {bytes,generation,kind,queued_at,latency_origin_at}=>{
                            if !handle.playback.accepts(generation){continue;}
                            let byte_count=bytes.len();
                            let started=Instant::now();
                            send_frame(&mut sender,Message::Binary(bytes)).await?;
                            transport.media(generation,kind,byte_count,queued_at,started,Instant::now(),latency_origin_at);
                            continue;
                        },
                        CallCommand::Mark {name,generation}=>{
                            if !handle.playback.accepts(generation){continue;}
                            Message::Text(json!({"type":"mark","name":name,"generation":generation}).to_string().into())
                        },
                        CallCommand::Clear=>Message::Text(json!({"type":"clear","generation":handle.playback.current_generation()}).to_string().into())
                    };
                    send_frame(&mut sender,message).await.map_err(|_|VoiceError::Protocol("connection_closed".into()))?;
                },
                _=heartbeat.tick()=>{send_frame(&mut sender,Message::Ping(Vec::new().into())).await.map_err(|_|VoiceError::Protocol("connection_closed".into()))?;}
            }
        }
        Ok(())
    }.await;
    let _ = tokio::time::timeout(Duration::from_secs(2), handle.input.send(CallEvent::Stop)).await;
    shutdown_playback_session(handle.task, &providers, &context).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), sender.close()).await;
    result
}

async fn send_frame<S>(sender: &mut S, message: Message) -> Result<(), VoiceError>
where
    S: futures_util::Sink<Message> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(1), sender.send(message))
        .await
        .map_err(|_| VoiceError::Timeout("desktop_write"))?
        .map_err(|_| VoiceError::Protocol("connection_closed".into()))
}
