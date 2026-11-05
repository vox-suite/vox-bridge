use std::collections::HashMap;

use actix_web::web;

use crate::state::AppState;

#[actix_web::post("/voice")]
async fn post_twilio_voice(
    data: web::Data<AppState>,
    form: web::Form<HashMap<String, String>>,
) -> impl actix_web::Responder {
    let current_session_id = uuid::Uuid::now_v7();

    let Some(_account_sid) = form.get("AccountSid") else {
        return actix_web::HttpResponse::BadRequest().body("AccountSid is required");
    };
    let Some(_call_sid) = form.get("CallSid") else {
        return actix_web::HttpResponse::BadRequest().body("CallSid is required");
    };

    let body = format!(
        r#"
        <?xml version="1.0" encoding="UTF-8"?>
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
        </Response>
        "#,
        data.vox_bridge_url, data.vox_bridge_url, current_session_id
    );

    actix_web::HttpResponse::Ok()
        .content_type("application/xml")
        .body(body)
}
