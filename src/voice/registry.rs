/**
* this file code contains voice runtime and provider registry initialization
*/
use std::{collections::HashMap, sync::Arc};

use crate::core::client::CoreClient;
use crate::providers::jev::JevClient;
use crate::providers::stt::assemblyai::AssemblyAiStt;
use crate::providers::tts::elevenlabs::{ElevenLabsSettings, ElevenLabsTts};
use crate::providers::tts::sarvam::{SarvamSettings, SarvamTts};
use crate::voice::config::{VoiceConfig, VoiceProfile};
use crate::voice::provider::{AgentProvider, SttProvider, TtsProvider, VoiceError};

pub trait VoiceProfileResolver: Send + Sync {
    fn resolve(&self) -> VoiceProfile;
}

pub struct StaticVoiceProfileResolver {
    pub profile: VoiceProfile,
}

impl VoiceProfileResolver for StaticVoiceProfileResolver {
    fn resolve(&self) -> VoiceProfile {
        self.profile.clone()
    }
}

pub struct ProviderRegistry {
    pub stt: HashMap<String, Arc<dyn SttProvider>>,
    pub agents: HashMap<String, Arc<dyn AgentProvider>>,
    pub tts: HashMap<String, Arc<dyn TtsProvider>>,
    pub filler_tts: Arc<dyn TtsProvider>,
    pub jev: Option<Arc<JevClient>>,
}

#[derive(Clone)]
pub struct ProviderSet {
    pub stt: Arc<dyn SttProvider>,
    pub agent: Arc<dyn AgentProvider>,
    pub tts: Arc<dyn TtsProvider>,
    pub filler_tts: Arc<dyn TtsProvider>,
    pub jev: Option<Arc<JevClient>>,
}

impl ProviderRegistry {
    pub fn providers_for(&self, profile: &VoiceProfile) -> Result<ProviderSet, VoiceError> {
        let stt = self
            .stt
            .get(&profile.stt.provider)
            .cloned()
            .ok_or_else(|| unregistered("STT", &profile.stt.provider))?;
        let agent = self
            .agents
            .get(&profile.agent.provider)
            .cloned()
            .ok_or_else(|| unregistered("agent", &profile.agent.provider))?;
        let tts = self
            .tts
            .get(&profile.tts.provider)
            .cloned()
            .ok_or_else(|| unregistered("TTS", &profile.tts.provider))?;
        Ok(ProviderSet {
            stt,
            agent,
            tts,
            filler_tts: self.filler_tts.clone(),
            jev: self.jev.clone(),
        })
    }
}

pub struct VoiceRuntime {
    pub providers: Arc<ProviderRegistry>,
    pub resolver: Arc<dyn VoiceProfileResolver>,
}

impl VoiceRuntime {
    pub fn from_config(config: VoiceConfig) -> Result<Self, VoiceError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .tcp_nodelay(true)
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|_| VoiceError::Configuration("HTTP client creation failed".into()))?;
        let profile = config.profile.clone();
        let stt: Arc<dyn SttProvider> = Arc::new(AssemblyAiStt::new(
            config.secrets.assemblyai_api_key,
            profile.stt.model.clone(),
        ));
        let agent: Arc<dyn AgentProvider> = Arc::new(
            CoreClient::new(config.secrets.core_url, config.secrets.core_auth_token)?
                .with_tts_provider(profile.tts.provider.clone()),
        );
        let tts: Arc<dyn TtsProvider> = match profile.tts.provider.as_str() {
            "sarvam" => {
                let api_key = config
                    .secrets
                    .sarvam_api_key
                    .ok_or_else(|| VoiceError::Configuration("SARVAM_API_KEY is missing".into()))?;
                Arc::new(SarvamTts::new(
                    http.clone(),
                    api_key,
                    "https://api.sarvam.ai".into(),
                    SarvamSettings {
                        model: profile.tts.model.clone(),
                        language_code: profile.tts.language_code.clone(),
                        speaker: profile.tts.speaker.clone(),
                        pace: profile.tts.pace,
                    },
                ))
            }
            "elevenlabs" => {
                let api_key = config.secrets.elevenlabs_api_key.as_ref().ok_or_else(|| {
                    VoiceError::Configuration("ELEVENLABS_API_KEY is missing".into())
                })?;
                let endpoint = "https://api.elevenlabs.io".to_string();
                let output_format = std::env::var("ELEVENLABS_OUTPUT_FORMAT")
                    .unwrap_or_else(|_| "mp3_44100_128".to_string());
                Arc::new(ElevenLabsTts::new(
                    http.clone(),
                    api_key.clone(),
                    endpoint,
                    ElevenLabsSettings {
                        model: profile.tts.model.clone(),
                        voice_id: profile.tts.speaker.clone(),
                        output_format,
                        voice_settings: None,
                    },
                ))
            }
            provider => {
                return Err(VoiceError::Configuration(format!(
                    "unsupported TTS provider {provider}"
                )));
            }
        };

        let filler_tts: Arc<dyn TtsProvider> = if profile.tts.provider == "elevenlabs" {
            if let (Some(filler_voice_id), Some(api_key)) = (
                config.secrets.elevenlabs_filler_voice_id.as_ref(),
                config.secrets.elevenlabs_api_key.as_ref(),
            ) {
                let endpoint = "https://api.elevenlabs.io".to_string();
                let output_format = std::env::var("ELEVENLABS_OUTPUT_FORMAT")
                    .unwrap_or_else(|_| "mp3_44100_128".to_string());
                Arc::new(ElevenLabsTts::new(
                    http.clone(),
                    api_key.clone(),
                    endpoint,
                    ElevenLabsSettings {
                        model: profile.tts.model.clone(),
                        voice_id: filler_voice_id.clone(),
                        output_format,
                        voice_settings: None,
                    },
                ))
            } else {
                tts.clone()
            }
        } else {
            tts.clone()
        };

        let jev = config
            .secrets
            .typesafe_api_key
            .map(|key| Arc::new(JevClient::new(http, key)));

        Ok(Self {
            providers: Arc::new(ProviderRegistry {
                stt: HashMap::from([(profile.stt.provider.clone(), stt)]),
                agents: HashMap::from([(profile.agent.provider.clone(), agent)]),
                tts: HashMap::from([(profile.tts.provider.clone(), tts)]),
                filler_tts,
                jev,
            }),
            resolver: Arc::new(StaticVoiceProfileResolver { profile }),
        })
    }
}

fn unregistered(kind: &str, provider: &str) -> VoiceError {
    VoiceError::Configuration(format!("unregistered {kind} provider {provider}"))
}
