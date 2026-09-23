/**
* this file code contains call and conversation channel context structures
*/
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelIdentity {
    pub channel: String,
    pub external_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    pub channel: String,
    pub external_identity: String,
    pub external_conversation_id: String,
    pub initiation_context: Option<String>,
    pub turn_id: Option<String>,
    pub revision: Option<u64>,
    pub tts_provider: Option<String>,
    pub filler: Option<String>,
}

pub fn normalized_e164(value: &str) -> Option<String> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if !(7..=15).contains(&digits.len()) {
        return None;
    }
    Some(format!("+{digits}"))
}
