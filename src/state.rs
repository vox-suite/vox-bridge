/**
* this file code contains application state shared across channels
*/
use dashmap::DashMap;
use std::sync::Arc;

use crate::channels::desktop::DesktopSessionState;
use crate::channels::notifications::{MessagingClient, NotificationDeliveryRecord, OptOutRecord};
use crate::channels::twilio::webhook::TwilioState;
use crate::core::client::CoreClient;
use crate::providers::telephony::TelephonyClient;
use crate::voice::registry::VoiceRuntime;

#[derive(Clone)]
pub struct AppState {
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
    pub desktop_sessions: Arc<DashMap<String, DesktopSessionState>>,
    pub opt_outs: Arc<DashMap<String, OptOutRecord>>,
    pub notification_deliveries: Arc<DashMap<String, NotificationDeliveryRecord>>,
    pub messaging_client: Option<Arc<dyn MessagingClient>>,
}
