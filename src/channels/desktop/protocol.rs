use crate::voice::provider::VoiceError;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Start { ticket: String },
    PlaybackFinished { name: String },
    Text { text: String },
    Stop,
}
pub fn parse_control(raw: &str) -> Result<Control, VoiceError> {
    if raw.len() > 8192 {
        return Err(VoiceError::Protocol("control_too_large".into()));
    }
    let control: Control =
        serde_json::from_str(raw).map_err(|_| VoiceError::Protocol("invalid_control".into()))?;
    match &control {
        Control::Start { ticket } if ticket.is_empty() || ticket.len() > 256 => {
            return Err(VoiceError::Protocol("invalid_ticket".into()));
        }
        Control::Text { text } if text.trim().is_empty() => {
            return Err(VoiceError::Protocol("empty_text".into()));
        }
        Control::PlaybackFinished { name } if name.len() > 128 => {
            return Err(VoiceError::Protocol("invalid_mark".into()));
        }
        _ => {}
    }
    Ok(control)
}
