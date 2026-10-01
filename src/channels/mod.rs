/**
* this file code contains channels modules and normalized context
*/
pub mod context;
pub mod interaction;
pub mod notifications;
pub mod outbound;
pub mod twilio;
pub mod verification;
pub mod whatsapp;

/// Helper to determine if live paid provider tests are explicitly enabled.
/// By default (in test and CI environments), this returns false to protect against
/// accidental usage of paid Twilio/WhatsApp/ElevenLabs APIs without an explicit flag.
pub fn is_live_channel_test_enabled() -> bool {
    std::env::var("VOX_LIVE_CHANNEL_TEST")
        .map(|v| live_channel_test_value(&v))
        .unwrap_or(false)
}

fn live_channel_test_value(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true")
}

#[cfg(test)]
mod live_test_config_tests {
    use super::live_channel_test_value;

    #[test]
    fn explicit_live_test_values_are_required() {
        for value in ["1", "true", "TRUE"] {
            assert!(live_channel_test_value(value));
        }
        for value in ["", "0", "false", "yes", " true "] {
            assert!(!live_channel_test_value(value));
        }
    }
}
