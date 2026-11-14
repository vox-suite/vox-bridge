use actix_ws::Session;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use vox_db::{
    PgPool,
    models::voice_sessions::VoiceSessionStatus,
    queries::voice_sessions::{twilio_session_matches, update_session_status},
};

pub async fn connected_handler(message: &Value, session: Session) -> bool {
    let supported = message["protocol"].as_str() == Some("Call");

    if !supported {
        let _ = session.close(None).await;
        return false;
    }

    true
}

pub async fn start_handler(
    message: &Value,
    session: Session,
    db: PgPool,
    active_stream: &mut Option<(uuid::Uuid, String)>,
) -> bool {
    if active_stream.is_some() {
        let _ = session.close(None).await;
        return false;
    }

    let start_message = &message["start"];
    let session_id = start_message["customParameters"]["session_id"]
        .as_str()
        .and_then(|value| uuid::Uuid::parse_str(value).ok());

    let (Some(session_id), Some(account_sid), Some(call_sid), Some(stream_sid)) = (
        session_id,
        start_message["accountSid"].as_str(),
        start_message["callSid"].as_str(),
        start_message["streamSid"].as_str(),
    ) else {
        let _ = session.close(None).await;
        return false;
    };

    if !matches!(
        twilio_session_matches(&db, session_id, account_sid, call_sid).await,
        Ok(true)
    ) {
        let _ = session.close(None).await;
        return false;
    }

    if let Err(error) = update_session_status(&db, session_id, VoiceSessionStatus::Active).await {
        eprintln!("Failed to activate session {session_id}: {error}");
        let _ = session.close(None).await;
        return false;
    }

    *active_stream = Some((session_id, stream_sid.to_owned()));
    true
}

pub async fn media_handler(
    message: &Value,
    session: Session,
    active_stream: &mut Option<(uuid::Uuid, String)>,
) -> bool {
    let Some((session_id, stream_sid)) = active_stream.as_ref() else {
        return false;
    };
    if message["streamSid"].as_str() != Some(stream_sid.as_str()) {
        let _ = session.close(None).await;
        return false;
    }

    let Some(payload) = message["media"]["payload"].as_str() else {
        let _ = session.close(None).await;
        return false;
    };

    let Ok(audio_bytes) = STANDARD.decode(payload) else {
        let _ = session.close(None).await;
        return false;
    };

    println!(
        "Session {session_id}: received {} audio bytes",
        audio_bytes.len()
    );

    true
}
