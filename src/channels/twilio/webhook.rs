use actix_web::{HttpRequest, HttpResponse, web};
use vox_db::queries::voice_sessions::get_or_create_twilio_session;

use super::signature::validate_twilio_signature;
use crate::state::AppState;

#[actix_web::post("/voice")]
pub async fn post_twilio_voice(
    data: web::Data<AppState>,
    request: HttpRequest,
    form: web::Form<Vec<(String, String)>>,
) -> impl actix_web::Responder {
    let Some(signature) = request
        .headers()
        .get("X-Twilio-Signature")
        .and_then(|value| value.to_str().ok())
    else {
        return HttpResponse::Unauthorized().finish();
    };

    let public_url = &data.twilio_voice_webhook_url;
    let configured_query = public_url.split_once('?').map(|(_, query)| query);
    if request.uri().query() != configured_query
        || !validate_twilio_signature(&data.twilio_auth_token, public_url, &form, signature)
    {
        return HttpResponse::Forbidden().finish();
    }

    let mut account_sids = form.iter().filter(|(name, _)| name == "AccountSid");
    let mut call_sids = form.iter().filter(|(name, _)| name == "CallSid");
    let (Some((_, account_sid)), Some((_, call_sid))) = (account_sids.next(), call_sids.next())
    else {
        return HttpResponse::BadRequest().body("AccountSid and CallSid are required");
    };
    if account_sid.is_empty()
        || call_sid.is_empty()
        || account_sids.next().is_some()
        || call_sids.next().is_some()
    {
        return HttpResponse::BadRequest().body("AccountSid and CallSid must each appear once");
    }

    let new_session_id = uuid::Uuid::now_v7();
    let session_id =
        match get_or_create_twilio_session(&data.db, account_sid, call_sid, new_session_id).await {
            Ok(session_id) => session_id,
            Err(_err) => {
                return HttpResponse::InternalServerError().body("Could not start the call");
            }
        };

    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
        <Response>
          <Connect>
            <Stream
              url="wss://{}/twilio/media"
              statusCallback="https://{}/twilio/stream-status"
              statusCallbackMethod="POST">
              <Parameter name="session_id" value="{}" />
                    </Stream>
          </Connect>
          <Hangup />
        </Response>"#,
        data.vox_bridge_url, data.vox_bridge_url, session_id
    );

    HttpResponse::Ok()
        .content_type("application/xml")
        .body(body)
}
