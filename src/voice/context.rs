#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    pub channel: String,
    pub external_identity: String,
    pub external_conversation_id: String,
    pub initiation_context: Option<String>,
    pub turn_id: Option<String>,
    pub revision: Option<u64>,
    pub voice_signature: Option<String>,
    pub tts_provider: Option<String>,
}
