use actix_web::{Error, HttpRequest, HttpResponse, rt, web};
use actix_ws::AggregatedMessage;
use futures_util::StreamExt as _;
use serde_json::Value;
use vox_db::{
    models::voice_sessions::VoiceSessionStatus, queries::voice_sessions::update_session_status,
};

use crate::{
    audio::assembly_ai,
    channels::twilio::{events_handlers, signature::validate_twilio_signature},
    state::AppState,
};

#[actix_web::get("/media")]
pub async fn twilio_media_websocket(
    req: HttpRequest,
    stream: web::Payload,
    data: web::Data<AppState>,
) -> Result<HttpResponse, Error> {
    let Some(signature) = req
        .headers()
        .get("X-Twilio-Signature")
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(HttpResponse::Unauthorized().finish());
    };

    let public_media_url = format!("wss://{}/twilio/media", data.vox_bridge_url);

    if !validate_twilio_signature(&data.twilio_auth_token, &public_media_url, &[], signature) {
        return Ok(HttpResponse::Forbidden().finish());
    }

    let (res, mut session, stream) = actix_ws::handle(&req, stream)?;
    let mut stream = stream
        .aggregate_continuations()
        .max_continuation_size(2_usize.pow(20));

    let db = data.db.clone();
    let assembly_ai_api_key = data.assembly_ai_api_key.clone();

    rt::spawn(async move {
        let mut active_stream: Option<(uuid::Uuid, String)> = None;
        let mut assembly_socket = None;
        let mut audio_buffer = Vec::new();
        let mut terminal_status = VoiceSessionStatus::Failed;

        loop {
            let msg = tokio::select! {
                msg = stream.next() => {
                    let Some(msg) = msg else { break };
                    msg
                }
                event = async {
                    match assembly_socket.as_mut() {
                        Some(socket) => assembly_ai::receive_event(socket).await,
                        None => std::future::pending().await,
                    }
                } => {
                    match event {
                        Ok(event) if event["type"].as_str() != Some("Termination") => {}
                        Ok(_) => {
                            eprintln!("AssemblyAI terminated before the call ended");
                            let _ = session.close(None).await;
                            break;
                        }
                        Err(error) => {
                            eprintln!("AssemblyAI receive failed: {error}");
                            let _ = session.close(None).await;
                            break;
                        }
                    }
                    continue;
                }
            };

            match msg {
                Ok(AggregatedMessage::Text(text)) => {
                    let Ok(message) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };

                    match message["event"].as_str() {
                        Some("connected") => {
                            if !events_handlers::connected_handler(&message, session.clone()).await
                            {
                                break;
                            }
                        }
                        Some("start") => {
                            if !events_handlers::start_handler(
                                &message,
                                session.clone(),
                                db.clone(),
                                &mut active_stream,
                            )
                            .await
                            {
                                break;
                            }

                            match assembly_ai::connect(&assembly_ai_api_key).await {
                                Ok(socket) => {
                                    assembly_socket = Some(socket);
                                    println!("AssemblyAI is ready for this call");
                                }
                                Err(error) => {
                                    eprintln!("AssemblyAI connection failed: {error}");
                                    let _ = session.close(None).await;
                                    break;
                                }
                            }
                        }
                        Some("media") => {
                            let Some(socket) = assembly_socket.as_mut() else {
                                let _ = session.close(None).await;
                                break;
                            };

                            if !events_handlers::media_handler(
                                &message,
                                session.clone(),
                                &mut active_stream,
                                socket,
                                &mut audio_buffer,
                            )
                            .await
                            {
                                break;
                            }
                        }
                        Some("stop") => {
                            terminal_status = VoiceSessionStatus::Ended;
                            let _ = session.close(None).await;
                            break;
                        }
                        _ => {}
                    }
                }
                Ok(AggregatedMessage::Ping(bytes)) => {
                    if session.pong(&bytes).await.is_err() {
                        let _ = session.close(None).await;
                        break;
                    }
                }
                Ok(AggregatedMessage::Close(reason)) => {
                    if reason
                        .as_ref()
                        .is_some_and(|reason| reason.code == actix_ws::CloseCode::Normal)
                    {
                        terminal_status = VoiceSessionStatus::Ended;
                    }
                    let _ = session.close(reason).await;
                    break;
                }
                Err(_) => {
                    let _ = session.close(None).await;
                    break;
                }
                _ => {}
            }
        }

        if let Some(socket) = assembly_socket.as_mut()
            && let Err(error) = assembly_ai::finish(socket, &mut audio_buffer).await
        {
            eprintln!("Failed to finish AssemblyAI session: {error}");
            terminal_status = VoiceSessionStatus::Failed;
        }

        if let Some((session_id, _)) = active_stream
            && let Err(error) = update_session_status(&db, session_id, terminal_status).await
        {
            eprintln!("Failed to finish session {session_id}: {error}");
        }
    });

    Ok(res)
}
