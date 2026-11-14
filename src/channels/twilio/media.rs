use actix_web::{Error, HttpRequest, HttpResponse, rt, web};
use actix_ws::AggregatedMessage;
use futures_util::StreamExt as _;
use serde_json::Value;

use crate::{
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

    let (res, session, stream) = actix_ws::handle(&req, stream)?;
    let mut stream = stream
        .aggregate_continuations()
        .max_continuation_size(2_usize.pow(20));

    let db = data.db.clone();

    rt::spawn(async move {
        let mut active_stream: Option<(uuid::Uuid, String)> = None;

        while let Some(msg) = stream.next().await {
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
                        }
                        _ => {}
                    }
                }

                _ => {}
            }
        }
    });

    Ok(res)
}
