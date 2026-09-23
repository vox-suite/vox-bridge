/**
* this file code contains bridge application configuration
*/
use crate::voice::config::VoiceConfig;
use crate::voice::provider::VoiceError;

#[derive(Clone)]
pub struct AppConfig {
    pub voice: VoiceConfig,
    pub twilio_account_sid: String,
    pub twilio_auth_token: String,
    pub twilio_from_number: String,
    pub service_token: String,
    pub port: u16,
    pub whatsapp_verify_token: Option<String>,
    pub whatsapp_app_secret: Option<String>,
    pub whatsapp_access_token: Option<String>,
    pub whatsapp_phone_id: Option<String>,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, VoiceError> {
        let voice = VoiceConfig::from_env()?;
        let twilio_account_sid = std::env::var("TWILIO_ACCOUNT_SID").unwrap_or_default();
        let twilio_auth_token = std::env::var("TWILIO_AUTH_TOKEN").unwrap_or_default();
        let twilio_from_number = std::env::var("TWILIO_FROM_NUMBER").unwrap_or_default();
        let service_token = std::env::var("VOX_AUTH_TOKEN").unwrap_or_default();
        let port = std::env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(3000);

        let whatsapp_verify_token = std::env::var("WA_VERIFY_KEY").ok();
        let whatsapp_app_secret = std::env::var("META_APP_SECRET").ok();
        let whatsapp_access_token = std::env::var("WHATSAPP_ACCESS_KEY").ok();
        let whatsapp_phone_id = std::env::var("WHATSAPP_PHONE_ID").ok();

        Ok(Self {
            voice,
            twilio_account_sid,
            twilio_auth_token,
            twilio_from_number,
            service_token,
            port,
            whatsapp_verify_token,
            whatsapp_app_secret,
            whatsapp_access_token,
            whatsapp_phone_id,
        })
    }
}
