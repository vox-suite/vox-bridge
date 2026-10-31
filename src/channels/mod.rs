use actix_web::web;

mod twilio;

pub fn channels_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/twilio").service(twilio::webhook::post_twilio_voice));
}
