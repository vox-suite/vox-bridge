use crate::voice::provider::VoiceError;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Start {
        ticket: String,
        #[serde(default)]
        client_metrics: bool,
        #[serde(default)]
        audio_formats: Vec<String>,
    },
    PlaybackFinished {
        name: String,
    },
    Text {
        text: String,
    },
    ClientMetric {
        stage: ClientMetricStage,
        elapsed_ms: u64,
        generation: u64,
        buffered_ms: u64,
    },
    Stop,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMetricStage {
    BootstrapReceived,
    SocketConnected,
    MicrophoneReady,
    SessionReady,
    FirstAudioReceived,
    FirstPlaybackStarted,
}
pub fn parse_control(raw: &str) -> Result<Control, VoiceError> {
    if raw.len() > 8192 {
        return Err(VoiceError::Protocol("control_too_large".into()));
    }
    let control: Control =
        serde_json::from_str(raw).map_err(|_| VoiceError::Protocol("invalid_control".into()))?;
    match &control {
        Control::Start { ticket, .. } if ticket.is_empty() || ticket.len() > 256 => {
            return Err(VoiceError::Protocol("invalid_ticket".into()));
        }
        Control::Text { text } if text.trim().is_empty() => {
            return Err(VoiceError::Protocol("empty_text".into()));
        }
        Control::PlaybackFinished { name } if name.len() > 128 => {
            return Err(VoiceError::Protocol("invalid_mark".into()));
        }
        Control::ClientMetric {
            elapsed_ms,
            buffered_ms,
            ..
        } if *elapsed_ms > 86_400_000 || *buffered_ms > 30_000 => {
            return Err(VoiceError::Protocol("invalid_metric".into()));
        }
        _ => {}
    }
    Ok(control)
}
