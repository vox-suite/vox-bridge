#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    pub channel: String,
    pub external_identity: String,
    pub external_conversation_id: String,
    pub initiation_context: Option<String>,
}
