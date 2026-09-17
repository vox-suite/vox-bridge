use crate::{
    agents::{
        core_client::CoreAgentClient,
        stt::assembly_stt::AssemblyAiStt,
        tts::sarvam_tts::{SarvamSettings, SarvamTts},
    },
    voice::{
        config::{VoiceConfig, VoiceProfile},
        provider::{AgentProvider, SttProvider, TtsProvider, VoiceError},
    },
};
use std::{collections::HashMap, sync::Arc};

pub trait VoiceProfileResolver: Send + Sync {
    fn resolve(&self) -> VoiceProfile;
}

pub struct StaticVoiceProfileResolver {
    profile: VoiceProfile,
}

impl VoiceProfileResolver for StaticVoiceProfileResolver {
    fn resolve(&self) -> VoiceProfile {
        self.profile.clone()
    }
}

pub struct ProviderRegistry {
    stt: HashMap<String, Arc<dyn SttProvider>>,
    agents: HashMap<String, Arc<dyn AgentProvider>>,
    tts: HashMap<String, Arc<dyn TtsProvider>>,
}

#[derive(Clone)]
pub struct ProviderSet {
    pub stt: Arc<dyn SttProvider>,
    pub agent: Arc<dyn AgentProvider>,
    pub tts: Arc<dyn TtsProvider>,
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
        Ok(ProviderSet { stt, agent, tts })
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
        let agent: Arc<dyn AgentProvider> = Arc::new(CoreAgentClient::new(
            config.secrets.core_url,
            config.secrets.core_service_token,
        )?);
        let tts: Arc<dyn TtsProvider> = Arc::new(SarvamTts::new(
            http,
            config.secrets.sarvam_api_key,
            "https://api.sarvam.ai".into(),
            SarvamSettings {
                model: profile.tts.model.clone(),
                language_code: profile.tts.language_code.clone(),
                speaker: profile.tts.speaker.clone(),
                pace: profile.tts.pace,
            },
        ));
        Ok(Self {
            providers: Arc::new(ProviderRegistry {
                stt: HashMap::from([(profile.stt.provider.clone(), stt)]),
                agents: HashMap::from([(profile.agent.provider.clone(), agent)]),
                tts: HashMap::from([(profile.tts.provider.clone(), tts)]),
            }),
            resolver: Arc::new(StaticVoiceProfileResolver { profile }),
        })
    }
}

fn unregistered(kind: &str, provider: &str) -> VoiceError {
    VoiceError::Configuration(format!("unregistered {kind} provider {provider}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::config::VoiceConfig;
    use std::collections::HashMap;

    fn config() -> VoiceConfig {
        let values = HashMap::from([
            ("ASSEMBLYAI_API_KEY".to_owned(), "assembly-key".to_owned()),
            ("VOX_CORE_URL".to_owned(), "http://core-api:3001".to_owned()),
            (
                "VOX_CORE_SERVICE_TOKEN".to_owned(),
                "service-token".to_owned(),
            ),
            ("SARVAM_API_KEY".to_owned(), "sarvam-key".to_owned()),
        ]);
        VoiceConfig::from_values(|key| values.get(key).cloned()).unwrap()
    }

    #[test]
    fn resolves_the_configured_provider_set() {
        let runtime = VoiceRuntime::from_config(config()).unwrap();
        let profile = runtime.resolver.resolve();

        assert_eq!(profile.stt.provider, "assemblyai");
        assert_eq!(profile.agent.provider, "vox-core");
        assert_eq!(profile.tts.provider, "sarvam");
        assert!(runtime.providers.providers_for(&profile).is_ok());
    }

    #[test]
    fn rejects_a_profile_for_an_unregistered_provider() {
        let runtime = VoiceRuntime::from_config(config()).unwrap();
        let mut profile = runtime.resolver.resolve();
        profile.tts.provider = "elevenlabs".into();

        let error = runtime.providers.providers_for(&profile).err().unwrap();

        assert!(error.to_string().contains("elevenlabs"));
    }
}
