// this file code contains voice configuration and profile definitions

use crate::voice::provider::VoiceError;
use std::collections::HashMap;

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

#[derive(Clone, Debug)]
pub struct VoiceSecrets {
    pub assemblyai_api_key: String,
    pub core_url: String,
    pub core_host_credential_id: String,
    pub core_host_audience: String,
    pub core_host_secret: String,
    pub sarvam_api_key: Option<String>,
    pub elevenlabs_api_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VoiceConfig {
    pub profile: VoiceProfile,
    pub secrets: VoiceSecrets,
}

impl VoiceConfig {
    pub fn from_env() -> Result<Self, VoiceError> {
        let values: HashMap<String, String> = [
            "VOX_STT_PROVIDER",
            "VOX_TTS_PROVIDER",
            "ASSEMBLYAI_API_KEY",
            "ASSEMBLYAI_SPEECH_MODEL",
            "VOX_CORE_HOST_CREDENTIAL_ID",
            "VOX_CORE_HOST_AUDIENCE",
            "VOX_CORE_HOST_SECRET",
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
        let core_host_credential_id = required(&get, "VOX_CORE_HOST_CREDENTIAL_ID")?;
        let core_host_audience = required(&get, "VOX_CORE_HOST_AUDIENCE")?;
        let core_host_secret = required(&get, "VOX_CORE_HOST_SECRET")?;

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
                        get("ELEVENLABS_API_KEY").filter(|v| !v.trim().is_empty()),
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
                core_host_credential_id,
                core_host_audience,
                core_host_secret,
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
    if speaker.trim().is_empty() {
        return Err(VoiceError::Configuration("speaker cannot be empty".into()));
    }
    let range = match model {
        "bulbul:v3" => 0.5..=2.0,
        "bulbul:v2" => 0.3..=3.0,
        _ => unreachable!(),
    };
    if !range.contains(&pace) {
        return Err(VoiceError::Configuration(format!(
            "pace {pace} is incompatible with {model}"
        )));
    }
    Ok(())
}
