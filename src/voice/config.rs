/**
* this file code contains voice configuration and profile definitions
*/
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
    pub core_auth_token: String,
    pub sarvam_api_key: Option<String>,
    pub elevenlabs_api_key: Option<String>,
    pub elevenlabs_filler_voice_id: Option<String>,
    pub typesafe_api_key: Option<String>,
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
            "VOX_CORE_URL",
            "ASSEMBLYAI_API_KEY",
            "ASSEMBLYAI_SPEECH_MODEL",
            "VOX_AUTH_TOKEN",
            "SARVAM_API_KEY",
            "ELEVENLABS_API_KEY",
            "ELEVENLABS_MODEL_ID",
            "ELEVENLABS_VOICE_ID",
            "ELEVENLABS_FILLER_VOICE_ID",
            "TYPESAFE_API_KEY",
            "JEV_API_KEY",
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
        let core_auth_token = required(&get, "VOX_AUTH_TOKEN")?;
        let typesafe_api_key = get("TYPESAFE_API_KEY")
            .or_else(|| get("JEV_API_KEY"))
            .filter(|v| !v.trim().is_empty());
        let elevenlabs_filler_voice_id =
            get("ELEVENLABS_FILLER_VOICE_ID").filter(|v| !v.trim().is_empty());

        let (tts_model, speaker, pace, language_code, sarvam_api_key, elevenlabs_api_key) =
            match tts_provider.as_str() {
                "sarvam" => {
                    let sarvam_api_key = required(&get, "SARVAM_API_KEY")?;
                    (
                        SARVAM_TTS_MODEL.to_string(),
                        SARVAM_SPEAKER.to_string(),
                        SARVAM_TTS_PACE,
                        SARVAM_LANGUAGE_CODE.to_string(),
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
                core_auth_token,
                sarvam_api_key,
                elevenlabs_api_key,
                elevenlabs_filler_voice_id,
                typesafe_api_key,
            },
        })
    }
}

fn value_or<F>(get: &F, key: &str, default: &str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    get(key).unwrap_or_else(|| default.to_string())
}

fn required<F>(get: &F, key: &'static str) -> Result<String, VoiceError>
where
    F: Fn(&str) -> Option<String>,
{
    get(key)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| VoiceError::Configuration(format!("{key} is missing")))
}

fn require_provider(kind: &str, provider: &str, supported: &[&str]) -> Result<(), VoiceError> {
    if supported.contains(&provider) {
        Ok(())
    } else {
        Err(VoiceError::Configuration(format!(
            "unsupported {kind} provider {provider}"
        )))
    }
}

fn default_core_url() -> String {
    "http://127.0.0.1:3001".to_string()
}

const SARVAM_TTS_MODEL: &str = "bulbul:v3";
const SARVAM_SPEAKER: &str = "shubh";
const SARVAM_LANGUAGE_CODE: &str = "en-IN";
const SARVAM_TTS_PACE: f64 = 1.0;
