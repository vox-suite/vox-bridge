// this file code contains tests for voice configuration validation and profiles

use std::collections::HashMap;
use vox_bridge::voice::config::VoiceConfig;

fn valid_values() -> HashMap<String, String> {
    HashMap::from([
        ("ASSEMBLYAI_API_KEY".into(), "assembly-key".into()),
        ("VOX_CORE_URL".into(), "http://core-api:3001".into()),
        (
            "VOX_CORE_HOST_CREDENTIAL_ID".into(),
            "f7f90d3d-5ded-4acf-850f-650bcb965fd1".into(),
        ),
        (
            "VOX_CORE_HOST_AUDIENCE".into(),
            "vox-host:development:bridge".into(),
        ),
        ("VOX_CORE_HOST_SECRET".into(), "host-secret".into()),
        ("ELEVENLABS_API_KEY".into(), "eleven-key".into()),
    ])
}

#[test]
fn applies_the_default_voice_profile() {
    let values = valid_values();
    let config = VoiceConfig::from_values(|k| values.get(k).cloned()).unwrap();
    assert_eq!(config.profile.stt.provider, "assemblyai");
    assert_eq!(config.profile.stt.model, "universal-3-5-pro");
    assert_eq!(config.profile.agent.provider, "vox-core");
    assert_eq!(config.profile.tts.provider, "elevenlabs");
    assert_eq!(config.profile.tts.model, "eleven_flash_v2_5");
    assert_eq!(config.profile.tts.speaker, "21m00Tcm4TlvDq8ikWAM");
    assert_eq!(config.secrets.core_url, "http://core-api:3001");
}

#[test]
fn validates_missing_required_credentials() {
    let values: HashMap<String, String> = HashMap::from([
        ("ASSEMBLYAI_API_KEY".into(), "assembly-key".into()),
    ]);
    let err = VoiceConfig::from_values(|k| values.get(k).cloned()).unwrap_err();
    assert!(err.to_string().contains("is missing"));
}

#[test]
fn supports_sarvam_tts_configuration() {
    let mut values = valid_values();
    values.insert("VOX_TTS_PROVIDER".into(), "sarvam".into());
    values.insert("SARVAM_API_KEY".into(), "sarvam-key".into());
    values.insert("SARVAM_TTS_MODEL".into(), "bulbul:v3".into());
    values.insert("SARVAM_SPEAKER".into(), "shubh".into());
    values.insert("SARVAM_TTS_PACE".into(), "1.1".into());

    let config = VoiceConfig::from_values(|k| values.get(k).cloned()).unwrap();
    assert_eq!(config.profile.tts.provider, "sarvam");
    assert_eq!(config.profile.tts.speaker, "shubh");
    assert_eq!(config.profile.tts.pace, 1.1);
}
