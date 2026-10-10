use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use vox_bridge::{
    channels::context::CallContext,
    voice::metrics::{ResponseMetrics, log_turn_latency},
};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn core_wait_is_not_reported_as_provider_llm_ttft() {
    let data = Arc::new(Mutex::new(Vec::new()));
    let writer = Capture(data.clone());
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let base = Instant::now();
    let context = CallContext {
        channel: "phone".into(),
        external_identity: "private-number".into(),
        external_conversation_id: "call-1".into(),
        initiation_context: None,
        turn_id: Some("turn-1".into()),
        revision: Some(2),
        tts_provider: None,
        filler: None,
    };
    tracing::subscriber::with_default(subscriber, || {
        let mut metrics = ResponseMetrics::new(base, None);
        metrics.core_requested = Some(base + Duration::from_millis(600));
        metrics.first_text = Some(base + Duration::from_millis(900));
        log_turn_latency(
            2,
            &context,
            1,
            "turn",
            &metrics,
            "completed",
            base + Duration::from_millis(1100),
        );
    });
    let log = String::from_utf8(data.lock().unwrap().clone()).unwrap();
    assert!(log.contains("core_first_text_ms=300"), "{log}");
    assert!(!log.contains("llm_ttft_ms"));
    assert!(!log.contains("private prompt"));
    assert!(!log.contains("private-number"));
}

struct PendingAgent;
#[async_trait::async_trait]
impl vox_bridge::core::ConversationClient for PendingAgent {
    async fn respond(
        &self,
        _: &CallContext,
        _: &str,
    ) -> Result<String, vox_bridge::voice::VoiceError> {
        std::future::pending().await
    }
}
struct SilentTts;
#[async_trait::async_trait]
impl vox_bridge::providers::tts::TtsProvider for SilentTts {
    async fn synthesize(
        &self,
        _: &str,
    ) -> Result<vox_bridge::providers::tts::AudioStream, vox_bridge::voice::VoiceError> {
        Ok(Box::pin(futures_util::stream::empty()))
    }
}
#[tokio::test]
async fn aborted_response_reports_cancellation_with_turn_correlation() {
    use std::sync::atomic::AtomicBool;
    use vox_bridge::voice::session::{PlaybackState, response::spawn_response};
    let data = Arc::new(Mutex::new(Vec::new()));
    let writer = Capture(data.clone());
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();
    let context = CallContext {
        channel: "phone".into(),
        external_identity: "private-number".into(),
        external_conversation_id: "call-cancel".into(),
        initiation_context: Some("private prompt".into()),
        turn_id: Some("greeting-1".into()),
        revision: Some(0),
        tts_provider: None,
        filler: None,
    };
    let playback = Arc::new(PlaybackState::new());
    playback.begin();
    let (out, _output) = tokio::sync::mpsc::channel(64);
    let (signal, _signals) = tokio::sync::mpsc::channel(8);
    let task = spawn_response(
        1,
        context.clone(),
        "private prompt".into(),
        None,
        Arc::new(PendingAgent),
        Arc::new(SilentTts),
        Arc::new(SilentTts),
        None,
        out,
        signal,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        playback,
    );
    for _ in 0..100 {
        if String::from_utf8_lossy(&data.lock().unwrap()).contains("processing turn") {
            break;
        }
        tokio::task::yield_now().await;
    }
    task.abort();
    let _ = task.await;
    let (out, mut output) = tokio::sync::mpsc::channel(64);
    let (signal, _signals) = tokio::sync::mpsc::channel(8);
    let playback = Arc::new(PlaybackState::new());
    playback.begin();
    let mut context = context;
    context.turn_id = Some("answer-1".into());
    let completed = spawn_response(
        1,
        context,
        "private prompt".into(),
        None,
        Arc::new(AnswerAgent),
        Arc::new(OneChunkTts),
        Arc::new(SilentTts),
        None,
        out,
        signal,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        playback,
    );
    tokio::time::timeout(Duration::from_secs(1), completed)
        .await
        .unwrap()
        .unwrap();
    let mut frames = 0;
    while let Ok(command) = output.try_recv() {
        if let vox_bridge::voice::session::CallCommand::Media {
            bytes,
            kind,
            latency_origin_at,
            ..
        } = command
        {
            assert!(!bytes.is_empty());
            assert_eq!(kind, vox_bridge::voice::session::AudioKind::Answer);
            assert!(latency_origin_at.is_some());
            frames += 1;
        }
    }
    assert_eq!(frames, 1);
    let logs = String::from_utf8(data.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("VOICE_TURN_LATENCY_REPORT"), "{logs}");
    assert!(logs.contains("outcome=\"cancelled\""), "{logs}");
    assert!(logs.contains("greeting-1"));
    assert!(logs.contains("outcome=\"completed\""), "{logs}");
    assert!(logs.contains("VOICE_TTS_SENTENCE"));
    assert!(!logs.contains("private-number"));
    assert!(!logs.contains("private prompt"));
}

struct AnswerAgent;
#[async_trait::async_trait]
impl vox_bridge::core::ConversationClient for AnswerAgent {
    async fn respond(
        &self,
        _: &CallContext,
        _: &str,
    ) -> Result<String, vox_bridge::voice::VoiceError> {
        Ok("Hello there.".into())
    }
}
struct OneChunkTts;
#[async_trait::async_trait]
impl vox_bridge::providers::tts::TtsProvider for OneChunkTts {
    async fn synthesize(
        &self,
        _: &str,
    ) -> Result<vox_bridge::providers::tts::AudioStream, vox_bridge::voice::VoiceError> {
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(bytes::Bytes::new()),
            Ok(bytes::Bytes::from_static(&[0xff; 160])),
        ])))
    }
}
