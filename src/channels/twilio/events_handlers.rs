use actix_ws::Session;
use serde_json::Value;
use vox_db::{PgPool, queries::voice_sessions::twilio_session_matches};

pub async fn connected_handler(message: &Value, session: Session) -> bool {
    let supported = message["protocol"].as_str() == Some("Call");

    if !supported {
        let _ = session.close(None).await;
        return false;
    }

    return true;
}

pub async fn start_handler(
    message: &Value,
    session: Session,
    db: PgPool,
    active_stream: &mut Option<(uuid::Uuid, String)>,
) -> bool {
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

    *active_stream = Some((session_id, stream_sid.to_owned()));
    return true;
}
