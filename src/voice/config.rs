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
    pub sarvam_api_key: String,
    pub jev_api_key: Option<String>,
    pub jev_base_url: Option<String>,
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
            "VOX_CORE_URL",
            "VOX_CORE_SERVICE_TOKEN",
            "SARVAM_API_KEY",
            "SARVAM_TTS_MODEL",
            "SARVAM_LANGUAGE_CODE",
            "SARVAM_SPEAKER",
            "SARVAM_TTS_PACE",
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
        let tts_provider = value_or(&get, "VOX_TTS_PROVIDER", "sarvam");
        require_provider("STT", &stt_provider, "assemblyai")?;
        require_provider("TTS", &tts_provider, "sarvam")?;

        let assemblyai_api_key = required(&get, "ASSEMBLYAI_API_KEY")?;
        let core_url = value_or(&get, "VOX_CORE_URL", "http://127.0.0.1:3001");
        let core_service_token = required(&get, "VOX_CORE_SERVICE_TOKEN")?;
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
            .map_err(|_| VoiceError::Configuration("SARVAM_TTS_PACE must be a number".into()))?;
        validate_sarvam(&tts_model, &speaker, pace)?;

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
                    language_code: value_or(&get, "SARVAM_LANGUAGE_CODE", "en-IN"),
                    speaker,
                    pace,
                },
            },
            secrets: VoiceSecrets {
                assemblyai_api_key,
                core_url,
                core_service_token,
                sarvam_api_key,
                jev_api_key: get("JEV")
                    .or_else(|| get("JEV_API_KEY"))
                    .filter(|v| !v.trim().is_empty()),
                jev_base_url: get("JEV_BASE_URL").filter(|v| !v.trim().is_empty()),
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

fn require_provider(kind: &str, actual: &str, supported: &str) -> Result<(), VoiceError> {
    if actual == supported {
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
            ("SARVAM_API_KEY".into(), "sarvam-key".into()),
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
            ("VOX_TTS_PROVIDER", "elevenlabs"),
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
    fn rejects_missing_provider_credentials() {
        for key in [
            "ASSEMBLYAI_API_KEY",
            "VOX_CORE_SERVICE_TOKEN",
            "SARVAM_API_KEY",
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
    fn rejects_invalid_sarvam_settings() {
        let mut values = valid_values();
        values.insert("SARVAM_TTS_PACE".into(), "2.1".into());
        assert!(VoiceConfig::from_values(|name| values.get(name).cloned()).is_err());

        let mut values = valid_values();
        values.insert("SARVAM_SPEAKER".into(), "anushka".into());
        assert!(VoiceConfig::from_values(|name| values.get(name).cloned()).is_err());
    }
}
