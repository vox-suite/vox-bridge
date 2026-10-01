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
    pub speaker: String,
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
    pub host_credential_id: Option<String>,
    pub host_audience: Option<String>,
    pub host_secret: Option<String>,
    pub agent_external_key: String,
    pub elevenlabs_api_key: String,
    pub elevenlabs_filler_voice_id: Option<String>,
    pub jev_api_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct VoiceConfig {
    pub profile: VoiceProfile,
    pub secrets: VoiceSecrets,
}

const STT_PROVIDER: &str = "assemblyai";
const STT_MODEL: &str = "universal-3-5-pro";
const TTS_PROVIDER: &str = "elevenlabs";
const TTS_MODEL: &str = "eleven_v4_turbo";
const DEFAULT_VOICE_ID: &str = "21m00Tcm4TlvDq8ikWAM";

impl VoiceConfig {
    pub fn from_env() -> Result<Self, VoiceError> {
        let values: HashMap<String, String> = [
            "VOX_CORE_URL",
            "ASSEMBLYAI_API_KEY",
            "VOX_AUTH_TOKEN",
            "VOX_HOST_CREDENTIAL_ID",
            "VOX_HOST_AUDIENCE",
            "VOX_HOST_SECRET",
            "VOX_AGENT_KEY",
            "ELEVENLABS_API_KEY",
            "ELEVENLABS_VOICE_ID",
            "ELEVENLABS_FILLER_VOICE_ID",
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
        let assemblyai_api_key = required(&get, "ASSEMBLYAI_API_KEY")?;
        let core_url = get("VOX_CORE_URL")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(default_core_url);
        let core_auth_token = required(&get, "VOX_AUTH_TOKEN")?;
        let elevenlabs_api_key = required(&get, "ELEVENLABS_API_KEY")?;
        let jev_api_key = get("JEV_API_KEY").filter(|v| !v.trim().is_empty());
        let elevenlabs_filler_voice_id =
            get("ELEVENLABS_FILLER_VOICE_ID").filter(|v| !v.trim().is_empty());
        let speaker = get("ELEVENLABS_VOICE_ID")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_VOICE_ID.to_string());

        Ok(Self {
            profile: VoiceProfile {
                stt: ProviderSelection {
                    provider: STT_PROVIDER.to_string(),
                    model: STT_MODEL.to_string(),
                },
                agent: ProviderSelection {
                    provider: "vox-core".into(),
                    model: "default".into(),
                },
                tts: TtsSelection {
                    provider: TTS_PROVIDER.to_string(),
                    model: TTS_MODEL.to_string(),
                    speaker,
                },
            },
            secrets: VoiceSecrets {
                assemblyai_api_key,
                core_url,
                core_auth_token,
                host_credential_id: get("VOX_HOST_CREDENTIAL_ID"),
                host_audience: get("VOX_HOST_AUDIENCE"),
                host_secret: get("VOX_HOST_SECRET"),
                agent_external_key: value_or(&get, "VOX_AGENT_KEY", "general"),
                elevenlabs_api_key,
                elevenlabs_filler_voice_id,
                jev_api_key,
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

fn default_core_url() -> String {
    "http://127.0.0.1:3001".to_string()
}
