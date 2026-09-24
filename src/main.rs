/**
* this file code contains bridge application entry point
*/
use dashmap::DashMap;
use std::sync::Arc;

use vox_bridge::channels::twilio::{TwilioApiClient, VOICE_STATUS_URL, VOICE_STREAM_URL};
use vox_bridge::core::client::CoreClient;
use vox_bridge::providers::telephony::TelephonyClient;
use vox_bridge::voice::config::VoiceConfig;
use vox_bridge::voice::filler::prewarm_fillers;
use vox_bridge::voice::registry::VoiceRuntime;
use vox_bridge::{AppState, app};

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    install_crypto_provider();
    tracing_subscriber::fmt::init();

    let twilio_state = Arc::new(DashMap::new());
    let voice_config = VoiceConfig::from_env().expect("voice provider configuration is invalid");
    let core_url = voice_config.secrets.core_url.clone();
    let auth_token = voice_config.secrets.core_auth_token.clone();

    let core_client = Arc::new(
        CoreClient::new(core_url.clone(), auth_token.clone())
            .expect("core client initialization failed"),
    );

    let voice = Arc::new(
        VoiceRuntime::from_config(voice_config)
            .expect("voice provider runtime initialization failed"),
    );

    let profile = voice.resolver.resolve();
    if let Ok(providers) = voice.providers.providers_for(&profile) {
        prewarm_fillers(providers.tts.clone());
    }

    let twilio_account_sid = std::env::var("TWILIO_ACCOUNT_SID").unwrap_or_default();
    let twilio_auth_token =
        std::env::var("TWILIO_AUTH_TOKEN").expect("TWILIO_AUTH_TOKEN is missing");
    let twilio_from_number = std::env::var("TWILIO_FROM_NUMBER").unwrap_or_default();

    let telephony: Option<Arc<dyn TelephonyClient>> =
        if !twilio_account_sid.is_empty() && !twilio_from_number.is_empty() {
            TwilioApiClient::new(
                twilio_account_sid.clone(),
                twilio_auth_token.clone(),
                twilio_from_number.clone(),
                VOICE_STREAM_URL.to_string(),
                VOICE_STATUS_URL.to_string(),
            )
            .ok()
            .map(|client| Arc::new(client) as Arc<dyn TelephonyClient>)
        } else {
            None
        };

    let service_token = auth_token;
    let whatsapp_verify_token = std::env::var("WA_VERIFY_KEY").ok();
    let whatsapp_app_secret = std::env::var("META_APP_SECRET").ok();
    let whatsapp_access_token = std::env::var("WHATSAPP_ACCESS_KEY").ok();
    let whatsapp_phone_id = std::env::var("WHATSAPP_PHONE_ID").ok();
    let desktop_sessions = Arc::new(DashMap::new());
    let opt_outs = Arc::new(DashMap::new());
    let notification_deliveries = Arc::new(DashMap::new());

    let app_state = Arc::new(AppState {
        twilio: twilio_state,
        twilio_account_sid: Arc::new(twilio_account_sid),
        twilio_auth_token: Arc::new(twilio_auth_token),
        twilio_from_number: Arc::new(twilio_from_number),
        service_token: Arc::new(service_token),
        core_url: Arc::new(core_url.clone()),
        telephony,
        voice,
        core_client,
        whatsapp_verify_token,
        whatsapp_app_secret,
        whatsapp_access_token,
        whatsapp_phone_id,
        desktop_sessions,
        opt_outs,
        notification_deliveries,
        messaging_client: None,
    });

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let prewarm_url = core_url.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Ok(client) = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            let _ = client
                .get(format!("{prewarm_url}/health/ready"))
                .send()
                .await;
        }
    });

    if let Err(e) = app::run_server(app_state, port).await {
        tracing::error!(error = %e, "Server failed to start");
    }
}

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
