/**
* this file code contains channels modules and normalized context
*/
pub mod context;
pub mod desktop;
pub mod interaction;
pub mod notifications;
pub mod outbound;
pub mod twilio;
pub mod whatsapp;

/// Helper to determine if live paid provider tests are explicitly enabled.
/// By default (in test and CI environments), this returns false to protect against
/// accidental usage of paid Twilio/WhatsApp/ElevenLabs APIs without an explicit flag.
pub fn is_live_channel_test_enabled() -> bool {
    std::env::var("VOX_LIVE_CHANNEL_TEST")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}
