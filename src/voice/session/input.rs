use super::CallEvent;
use crate::voice::provider::VoiceError;
use std::time::Instant;

pub(crate) fn admit_input(
    input: &tokio::sync::mpsc::Sender<CallEvent>,
    event: CallEvent,
) -> Result<Option<(CallEvent, Instant)>, VoiceError> {
    match input.try_send(event) {
        Ok(()) => Ok(None),
        Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
            tracing::warn!(
                queue_capacity = input.max_capacity(),
                queue_depth = input.max_capacity() - input.capacity(),
                "VOICE_INPUT_BACKPRESSURE"
            );
            Ok(Some((event, Instant::now())))
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            tracing::warn!("VOICE_INPUT_RECEIVER_CLOSED");
            Err(VoiceError::Protocol("voice input receiver closed".into()))
        }
    }
}
