use crate::voice::provider::VoiceError;
use std::collections::HashMap;

const V3_SPEAKERS: &[&str] = &[
    "shubh", "aditya", "ritu", "priya", "neha", "rahul", "pooja", "rohan", "simran", "kavya",
    "amit", "dev", "ishita", "shreya", "ratan", "varun", "manan", "sumit", "roopa", "kabir",
    "aayan", "ashutosh", "advait", "anand", "tarun", "sunny", "mani", "gokul", "vijay", "shruti",
    "suhani", "mohit", "kavitha", "rehan", "soham", "rupali",
];
const V2_SPEAKERS: &[&str] = &[
    "anushka", "manisha", "vidya", "arya", "abhilash", "karun", "hitesh",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSelection {
    pub provider: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TtsSelection {
    pub provider: String,
    pub model: String,
    pub language_code: String,
    pub speaker: String,
    pub pace: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VoiceProfile {
    pub stt: ProviderSelection,
    pub agent: ProviderSelection,
    pub tts: TtsSelection,
}

#[derive(Clone)]
pub(crate) struct VoiceSecrets {
    pub assemblyai_api_key: String,
    pub core_url: String,
    pub core_service_token: String,
    pub sarvam_api_key: Option<String>,
    pub elevenlabs_api_key: Option<String>,
}

#[derive(Clone)]
pub struct VoiceConfig {
    pub profile: VoiceProfile,
    pub(crate) secrets: VoiceSecrets,
}

impl VoiceConfig {
    pub fn from_env() -> Result<Self, VoiceError> {
        let values: HashMap<String, String> = [
            "VOX_STT_PROVIDER",
            "VOX_TTS_PROVIDER",
            "ASSEMBLYAI_API_KEY",
            "ASSEMBLYAI_SPEECH_MODEL",
            "VOX_CORE_SERVICE_TOKEN",
            "SARVAM_API_KEY",
            "SARVAM_TTS_MODEL",
            "SARVAM_LANGUAGE_CODE",
            "SARVAM_SPEAKER",
            "SARVAM_TTS_PACE",
            "ELEVENLABS_API_KEY",
            "ELEVENLABS_MODEL_ID",
            "ELEVENLABS_VOICE_ID",
        ]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
        .collect();
        Self::from_values(|key| values.get(key).cloned())
    }

    pub fn from_values<F>(get: F) -> Result<Self, VoiceError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let stt_provider = value_or(&get, "VOX_STT_PROVIDER", "assemblyai");
        let tts_provider = value_or(&get, "VOX_TTS_PROVIDER", "elevenlabs");
        require_provider("STT", &stt_provider, &["assemblyai"])?;
        require_provider("TTS", &tts_provider, &["sarvam", "elevenlabs"])?;

        let assemblyai_api_key = required(&get, "ASSEMBLYAI_API_KEY")?;
        let core_url = get("VOX_CORE_URL")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(default_core_url);
        let core_service_token = required(&get, "VOX_CORE_SERVICE_TOKEN")?;

        let (tts_model, speaker, pace, language_code, sarvam_api_key, elevenlabs_api_key) =
            match tts_provider.as_str() {
                "sarvam" => {
                    let sarvam_api_key = required(&get, "SARVAM_API_KEY")?;
                    let tts_model = value_or(&get, "SARVAM_TTS_MODEL", "bulbul:v3");
                    let default_speaker = match tts_model.as_str() {
                        "bulbul:v3" => "shubh",
                        "bulbul:v2" => "anushka",
                        model => {
                            return Err(VoiceError::Configuration(format!(
                                "unsupported Sarvam model {model}"
                            )));
                        }
                    };
                    let speaker = value_or(&get, "SARVAM_SPEAKER", default_speaker);
                    let pace = value_or(&get, "SARVAM_TTS_PACE", "1.0")
                        .parse::<f64>()
                        .map_err(|_| {
                            VoiceError::Configuration("SARVAM_TTS_PACE must be a number".into())
                        })?;
                    validate_sarvam(&tts_model, &speaker, pace)?;
                    (
                        tts_model,
                        speaker,
                        pace,
                        value_or(&get, "SARVAM_LANGUAGE_CODE", "en-IN"),
                        Some(sarvam_api_key),
                        get("ELEVENLABS_API_KEY")
                            .filter(|v| !v.trim().is_empty()),
                    )
                }
                "elevenlabs" => {
                    let elevenlabs_api_key = get("ELEVENLABS_API_KEY")
                        .filter(|v| !v.trim().is_empty())
                        .ok_or_else(|| {
                            VoiceError::Configuration("ELEVENLABS_API_KEY is missing".into())
                        })?;
                    let tts_model = get("ELEVENLABS_MODEL_ID")
                        .filter(|v| !v.trim().is_empty())
                        .unwrap_or_else(|| "eleven_flash_v2_5".to_string());
                    let speaker = get("ELEVENLABS_VOICE_ID")
                        .filter(|v| !v.trim().is_empty())
                        .unwrap_or_else(|| "21m00Tcm4TlvDq8ikWAM".to_string());
                    (
                        tts_model,
                        speaker,
                        1.0,
                        "".to_string(),
                        get("SARVAM_API_KEY").filter(|v| !v.trim().is_empty()),
                        Some(elevenlabs_api_key),
                    )
                }
                _ => unreachable!(),
            };

        Ok(Self {
            profile: VoiceProfile {
                stt: ProviderSelection {
                    provider: stt_provider,
                    model: value_or(&get, "ASSEMBLYAI_SPEECH_MODEL", "universal-3-5-pro"),
                },
                agent: ProviderSelection {
                    provider: "vox-core".into(),
                    model: "default".into(),
                },
                tts: TtsSelection {
                    provider: tts_provider,
                    model: tts_model,
                    language_code,
                    speaker,
                    pace,
                },
            },
            secrets: VoiceSecrets {
                assemblyai_api_key,
                core_url,
                core_service_token,
                sarvam_api_key,
                elevenlabs_api_key,
            },
        })
    }
}

fn value_or<F>(get: &F, key: &str, default: &str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    get(key)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn required<F>(get: &F, key: &str) -> Result<String, VoiceError>
where
    F: Fn(&str) -> Option<String>,
{
    get(key)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| VoiceError::Configuration(format!("{key} is missing")))
}

fn default_core_url() -> String {
    use std::net::ToSocketAddrs;
    if ("core-api", 3001).to_socket_addrs().is_ok() {
        "http://core-api:3001".to_string()
    } else {
        "http://127.0.0.1:3001".to_string()
    }
}

fn require_provider(kind: &str, actual: &str, supported: &[&str]) -> Result<(), VoiceError> {
    if supported.contains(&actual) {
        Ok(())
    } else {
        Err(VoiceError::Configuration(format!(
            "unsupported {kind} provider {actual}"
        )))
    }
}

fn validate_sarvam(model: &str, speaker: &str, pace: f64) -> Result<(), VoiceError> {
    let (speakers, range) = match model {
        "bulbul:v3" => (V3_SPEAKERS, 0.5..=2.0),
        "bulbul:v2" => (V2_SPEAKERS, 0.3..=3.0),
        _ => unreachable!(),
    };
    if !speakers.contains(&speaker) {
        return Err(VoiceError::Configuration(format!(
            "speaker {speaker} is incompatible with {model}"
        )));
    }
    if !range.contains(&pace) {
        return Err(VoiceError::Configuration(format!(
            "pace {pace} is incompatible with {model}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn valid_values() -> HashMap<String, String> {
        HashMap::from([
            ("ASSEMBLYAI_API_KEY".into(), "assembly-key".into()),
            ("VOX_CORE_URL".into(), "http://core-api:3001".into()),
            ("VOX_CORE_SERVICE_TOKEN".into(), "service-token".into()),
            ("ELEVENLABS_API_KEY".into(), "eleven-key".into()),
        ])
    }

    #[test]
    fn applies_the_default_voice_profile() {
        let values = valid_values();
        let config = VoiceConfig::from_values(|key| values.get(key).cloned()).unwrap();

        assert_eq!(config.profile.stt.provider, "assemblyai");
        assert_eq!(config.profile.stt.model, "universal-3-5-pro");
        assert_eq!(config.profile.agent.provider, "vox-core");
        assert_eq!(config.profile.agent.model, "default");
        assert_eq!(config.profile.tts.provider, "elevenlabs");
        assert_eq!(config.profile.tts.model, "eleven_flash_v2_5");
        assert_eq!(config.profile.tts.speaker, "21m00Tcm4TlvDq8ikWAM");
        assert_eq!(config.profile.tts.pace, 1.0);
    }

    #[test]
    fn configures_sarvam_voice_profile() {
        let mut values = valid_values();
        values.remove("ELEVENLABS_API_KEY");
        values.insert("VOX_TTS_PROVIDER".into(), "sarvam".into());
        values.insert("SARVAM_API_KEY".into(), "sarvam-key".into());

        let config = VoiceConfig::from_values(|key| values.get(key).cloned()).unwrap();

        assert_eq!(config.profile.tts.provider, "sarvam");
        assert_eq!(config.profile.tts.model, "bulbul:v3");
        assert_eq!(config.profile.tts.language_code, "en-IN");
        assert_eq!(config.profile.tts.speaker, "shubh");
        assert_eq!(config.profile.tts.pace, 1.0);
    }

    #[test]
    fn defaults_core_url_for_native_deployment() {
        let mut values = valid_values();
        values.remove("VOX_CORE_URL");

        let config = VoiceConfig::from_values(|key| values.get(key).cloned()).unwrap();

        assert_eq!(config.secrets.core_url, "http://127.0.0.1:3001");
    }

    #[test]
    fn rejects_unsupported_providers() {
        for (key, value) in [
            ("VOX_STT_PROVIDER", "deepgram"),
            ("VOX_TTS_PROVIDER", "unsupported_tts"),
        ] {
            let mut values = valid_values();
            values.insert(key.into(), value.into());

            let error = VoiceConfig::from_values(|name| values.get(name).cloned())
                .err()
                .unwrap();

            assert!(error.to_string().contains(value));
        }
    }

    #[test]
    fn configures_elevenlabs_voice_profile() {
        let mut values = valid_values();
        values.insert("VOX_TTS_PROVIDER".into(), "elevenlabs".into());
        values.insert("ELEVENLABS_API_KEY".into(), "eleven-key".into());

        let config = VoiceConfig::from_values(|key| values.get(key).cloned()).unwrap();

        assert_eq!(config.profile.tts.provider, "elevenlabs");
        assert_eq!(config.profile.tts.model, "eleven_flash_v2_5");
        assert_eq!(config.profile.tts.speaker, "21m00Tcm4TlvDq8ikWAM");
        assert_eq!(
            config.secrets.elevenlabs_api_key.as_deref(),
            Some("eleven-key")
        );
    }

    #[test]
    fn configures_custom_elevenlabs_settings() {
        let mut values = valid_values();
        values.insert("VOX_TTS_PROVIDER".into(), "elevenlabs".into());
        values.insert("ELEVENLABS_API_KEY".into(), "custom-xi-key".into());
        values.insert("ELEVENLABS_MODEL_ID".into(), "eleven_multilingual_v2".into());
        values.insert("ELEVENLABS_VOICE_ID".into(), "custom-voice-id".into());

        let config = VoiceConfig::from_values(|key| values.get(key).cloned()).unwrap();

        assert_eq!(config.profile.tts.provider, "elevenlabs");
        assert_eq!(config.profile.tts.model, "eleven_multilingual_v2");
        assert_eq!(config.profile.tts.speaker, "custom-voice-id");
        assert_eq!(
            config.secrets.elevenlabs_api_key.as_deref(),
            Some("custom-xi-key")
        );

    }

    #[test]
    fn rejects_missing_provider_credentials() {
        for key in [
            "ASSEMBLYAI_API_KEY",
            "VOX_CORE_SERVICE_TOKEN",
            "ELEVENLABS_API_KEY",
        ] {
            let mut values = valid_values();
            values.remove(key);

            let error = VoiceConfig::from_values(|name| values.get(name).cloned())
                .err()
                .unwrap();

            assert!(error.to_string().contains(key));
        }
    }

    #[test]
    fn rejects_missing_sarvam_credentials() {
        let mut values = valid_values();
        values.remove("ELEVENLABS_API_KEY");
        values.insert("VOX_TTS_PROVIDER".into(), "sarvam".into());

        let error = VoiceConfig::from_values(|name| values.get(name).cloned())
            .err()
            .unwrap();

        assert!(error.to_string().contains("SARVAM_API_KEY"));
    }

    #[test]
    fn rejects_invalid_sarvam_settings() {
        let mut values = valid_values();
        values.remove("ELEVENLABS_API_KEY");
        values.insert("VOX_TTS_PROVIDER".into(), "sarvam".into());
        values.insert("SARVAM_API_KEY".into(), "sarvam-key".into());
        values.insert("SARVAM_TTS_PACE".into(), "2.1".into());
        assert!(VoiceConfig::from_values(|name| values.get(name).cloned()).is_err());

        let mut values = valid_values();
        values.remove("ELEVENLABS_API_KEY");
        values.insert("VOX_TTS_PROVIDER".into(), "sarvam".into());
        values.insert("SARVAM_API_KEY".into(), "sarvam-key".into());
        values.insert("SARVAM_SPEAKER".into(), "anushka".into());
        assert!(VoiceConfig::from_values(|name| values.get(name).cloned()).is_err());
    }
}
