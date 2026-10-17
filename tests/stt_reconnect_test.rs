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
        outbound: false,
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

    assert_eq!(connects.load(Ordering::SeqCst), 4);
}

struct FlakyReconnect(Arc<AtomicU32>);
struct WaitingSession;
#[async_trait]
impl SttSession for WaitingSession {
    async fn send_audio(&self, _: Bytes) -> Result<(), VoiceError> {
        Ok(())
    }
    async fn next_event(&self) -> Result<Option<SttEvent>, VoiceError> {
        std::future::pending().await
    }
    async fn finish(&self) -> Result<(), VoiceError> {
        Ok(())
    }
}
#[async_trait]
impl SttProvider for FlakyReconnect {
    async fn connect(&self) -> Result<Arc<dyn SttSession>, VoiceError> {
        match self.0.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(Arc::new(ClosedSession)),
            1 => Err(VoiceError::Timeout("transient connect")),
            _ => Ok(Arc::new(WaitingSession)),
        }
    }
}
fn context() -> CallContext {
    CallContext {
        channel: "voice".into(),
        external_identity: "test".into(),
        external_conversation_id: "test".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        outbound: false,
    }
}
#[tokio::test]
async fn failed_reconnect_is_retried_before_ending_call() {
    let connects = Arc::new(AtomicU32::new(0));
    let providers = VoiceProviders {
        stt: Arc::new(FlakyReconnect(connects.clone())),
        agent: Arc::new(NoAgent),
        tts: Arc::new(NoTts),
        filler_tts: Arc::new(NoTts),
        jev: None,
    };
    let (tx, rx) = mpsc::channel(8);
    let (out, _receiver) = mpsc::channel(8);
    let task = tokio::spawn(run_voice_session(providers, context(), rx, out));
    tokio::time::timeout(Duration::from_secs(3), async {
        while connects.load(Ordering::SeqCst) < 3 && !task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tx.send(vox_bridge::voice::session::CallEvent::Stop)
        .await
        .ok();
    assert!(
        task.await.unwrap().is_ok(),
        "transient reconnect failure must not end the call"
    );
    assert_eq!(connects.load(Ordering::SeqCst), 3);
}
struct PendingAgent {
    entered: Arc<tokio::sync::Notify>,
    dropped: Arc<tokio::sync::Notify>,
}
struct NotifyOnDrop(Arc<tokio::sync::Notify>);
impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}
#[async_trait]
impl ConversationClient for PendingAgent {
    async fn respond(&self, _: &CallContext, _: &str) -> Result<String, VoiceError> {
        let _guard = NotifyOnDrop(self.dropped.clone());
        self.entered.notify_one();
        std::future::pending().await
    }
}
struct FailAfterGreeting(Arc<tokio::sync::Notify>);
#[async_trait]
impl SttProvider for FailAfterGreeting {
    async fn connect(&self) -> Result<Arc<dyn SttSession>, VoiceError> {
        self.0.notified().await;
        Err(VoiceError::Timeout("initial connect"))
    }
}
#[tokio::test]
async fn failing_session_cancels_its_pending_greeting() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(tokio::sync::Notify::new());
    let providers = VoiceProviders {
        stt: Arc::new(FailAfterGreeting(entered.clone())),
        agent: Arc::new(PendingAgent {
            entered,
            dropped: dropped.clone(),
        }),
        tts: Arc::new(NoTts),
        filler_tts: Arc::new(NoTts),
        jev: None,
    };
    let mut ctx = context();
    ctx.initiation_context = Some("greet".into());
    let (_tx, rx) = mpsc::channel(8);
    let (out, _receiver) = mpsc::channel(8);
    assert!(run_voice_session(providers, ctx, rx, out).await.is_err());
    tokio::time::timeout(Duration::from_millis(300), dropped.notified())
        .await
        .expect("session error must cancel the owned response");
}

struct HealthyStt;
#[async_trait]
impl SttProvider for HealthyStt {
    async fn connect(&self) -> Result<Arc<dyn SttSession>, VoiceError> {
        Ok(Arc::new(WaitingSession))
    }
}
#[tokio::test]
async fn aborting_session_cancels_its_pending_response() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(tokio::sync::Notify::new());
    let providers = VoiceProviders {
        stt: Arc::new(HealthyStt),
        agent: Arc::new(PendingAgent {
            entered: entered.clone(),
            dropped: dropped.clone(),
        }),
        tts: Arc::new(NoTts),
        filler_tts: Arc::new(NoTts),
        jev: None,
    };
    let mut ctx = context();
    ctx.initiation_context = Some("greet".into());
    let (_tx, rx) = mpsc::channel(8);
    let (out, _receiver) = mpsc::channel(8);
    let task = tokio::spawn(run_voice_session(providers, ctx, rx, out));
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_millis(300), dropped.notified())
        .await
        .expect("aborting the owner must cancel its response");
}
