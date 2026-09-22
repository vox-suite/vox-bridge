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

pub fn normalized_e164(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().any(|character| {
            !character.is_ascii_digit()
                && !character.is_ascii_whitespace()
                && !matches!(character, '+' | '-' | '(' | ')')
        })
        || value.matches('+').count() > 1
        || (value.contains('+') && !value.starts_with('+'))
    {
        return None;
    }
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if !(7..=15).contains(&digits.len()) {
        return None;
    }
    Some(format!("+{digits}"))
}

#[cfg(test)]
mod tests {
    use super::normalized_e164;

    #[test]
    fn accepts_normalizable_numbers_and_rejects_missing_or_malformed_senders() {
        assert_eq!(
            normalized_e164("+91 98765-43210"),
            Some("+919876543210".into())
        );
        assert_eq!(normalized_e164(""), None);
        assert_eq!(normalized_e164("unknown"), None);
        assert_eq!(normalized_e164("caller1234567"), None);
        assert_eq!(normalized_e164("123"), None);
        assert_eq!(normalized_e164("1234567890123456"), None);
    }
}
