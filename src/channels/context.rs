#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelIdentity {
    pub channel: String,
    pub external_id: String,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct VoiceUsageReport {
    pub session_seconds: f64,
    pub input_audio_seconds: f64,
    pub output_audio_seconds: f64,
}

#[derive(Clone, Debug, Default)]
pub struct VoiceUsage(std::sync::Arc<std::sync::OnceLock<VoiceUsageReport>>);

impl VoiceUsage {
    pub fn set(&self, report: VoiceUsageReport) {
        let _ = self.0.set(report);
    }

    pub fn get(&self) -> Option<VoiceUsageReport> {
        self.0.get().copied()
    }
}

#[derive(Clone, Debug)]
pub struct CallContext {
    pub channel: String,
    pub external_identity: String,
    pub external_conversation_id: String,
    pub initiation_context: Option<String>,
    pub turn_id: Option<String>,
    pub revision: Option<u64>,
    pub tts_provider: Option<String>,
    pub outbound: bool,
    pub usage: VoiceUsage,
}

pub fn normalized_e164(value: &str) -> Option<String> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if !(7..=15).contains(&digits.len()) {
        return None;
    }
    Some(format!("+{digits}"))
}
