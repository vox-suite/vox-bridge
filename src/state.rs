use std::env;
use vox_db::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub vox_bridge_url: String,
    pub twilio_auth_token: String,
    pub twilio_voice_webhook_url: String,
    pub assembly_ai_api_key: String,
    pub db: PgPool,
}

pub fn init_actix_app_state(db: PgPool) -> AppState {
    let vox_bridge_url =
        env::var("VOX_BRIDGE_URL").expect("VOX_BRIDGE_URL must be set in the environment");
    let twilio_auth_token = env::var("TWILIO_AUTH_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty())
        .expect("TWILIO_AUTH_TOKEN must be set and non-empty");
    let twilio_voice_webhook_url = env::var("TWILIO_VOICE_WEBHOOK_URL")
        .unwrap_or_else(|_| format!("https://{vox_bridge_url}/twilio/voice"));
    let assembly_ai_api_key = env::var("ASSEMBLY_AI_API_KEY")
        .expect("ASSEMBLY_AI_API_KEY must be set in the environment");

    AppState {
        vox_bridge_url,
        twilio_auth_token,
        twilio_voice_webhook_url,
        assembly_ai_api_key,
        db,
    }
}
