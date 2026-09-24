/**
* this file code contains a regression test for STT stream loss during a voice session
*/
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

use vox_bridge::channels::context::CallContext;
use vox_bridge::providers::stt::{SttEvent, SttProvider, SttSession};
use vox_bridge::providers::tts::{AudioStream, TtsProvider};
use vox_bridge::voice::provider::{ConversationClient, VoiceError, VoiceProviders};
use vox_bridge::voice::session::run_voice_session;

/// Every session it hands out is already closed.
struct ClosedStt(Arc<AtomicU32>);
struct ClosedSession;

#[async_trait]
impl SttSession for ClosedSession {
    async fn send_audio(&self, _audio: Bytes) -> Result<(), VoiceError> {
        Ok(())
    }
    async fn next_event(&self) -> Result<Option<SttEvent>, VoiceError> {
        Ok(None)
    }
    async fn finish(&self) -> Result<(), VoiceError> {
        Ok(())
    }
}

#[async_trait]
impl SttProvider for ClosedStt {
    async fn connect(&self) -> Result<Arc<dyn SttSession>, VoiceError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(ClosedSession))
    }
}

struct NoAgent;
#[async_trait]
impl ConversationClient for NoAgent {
    async fn respond(&self, _c: &CallContext, _t: &str) -> Result<String, VoiceError> {
        Ok(String::new())
    }
}

struct NoTts;
#[async_trait]
impl TtsProvider for NoTts {
    async fn synthesize(&self, _text: &str) -> Result<AudioStream, VoiceError> {
        Err(VoiceError::Protocol("unused".into()))
    }
}

#[tokio::test]
async fn closed_stt_stream_reconnects_then_ends_instead_of_spinning() {
    let connects = Arc::new(AtomicU32::new(0));
    let providers = VoiceProviders {
        stt: Arc::new(ClosedStt(connects.clone())),
        agent: Arc::new(NoAgent),
        tts: Arc::new(NoTts),
        filler_tts: Arc::new(NoTts),
        jev: None,
    };
    let context = CallContext {
        channel: "desktop".into(),
        external_identity: "vox-account:test".into(),
        external_conversation_id: "conv".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };
    let (_input_tx, input_rx) = mpsc::channel(8);
    let (output_tx, _output_rx) = mpsc::channel(8);

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_voice_session(providers, context, input_rx, output_tx),
    )
    .await
    .expect("session must end, not spin forever on a closed STT stream");

    assert!(result.is_err());
    // Initial connect plus three reconnect attempts.
    assert_eq!(connects.load(Ordering::SeqCst), 4);
}
