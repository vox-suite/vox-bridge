// this file code contains application state shared across channels

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::channels::twilio::webhook::TwilioState;
use crate::core::client::CoreClient;
use crate::providers::telephony::TelephonyClient;
use crate::voice::registry::VoiceRuntime;

pub struct AppState {
    pub tx: broadcast::Sender<String>,
    pub twilio: Arc<DashMap<String, TwilioState>>,
    pub twilio_account_sid: Arc<String>,
    pub twilio_auth_token: Arc<String>,
    pub twilio_from_number: Arc<String>,
    pub service_token: Arc<String>,
    pub core_url: Arc<String>,
    pub telephony: Option<Arc<dyn TelephonyClient>>,
    pub voice: Arc<VoiceRuntime>,
    pub core_client: Arc<CoreClient>,
    pub whatsapp_verify_token: Option<String>,
    pub whatsapp_app_secret: Option<String>,
    pub whatsapp_access_token: Option<String>,
    pub whatsapp_phone_id: Option<String>,
}
