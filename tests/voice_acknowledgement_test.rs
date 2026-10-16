use async_trait::async_trait;
use axum::{Json, Router, routing::post};
use bytes::Bytes;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use tokio::sync::{Notify, mpsc};
use vox_bridge::{
    channels::context::CallContext,
    core::ConversationClient,
    providers::{
        jev::JevClient,
        tts::{AudioStream, TtsProvider},
    },
    voice::{
        metrics::TurnTiming,
        provider::VoiceError,
        session::{AudioKind, CallCommand, PlaybackState, response::spawn_response},
    },
};

struct Agent {
    delay: Duration,
    started: Arc<Notify>,
    failed: bool,
}
#[async_trait]
impl ConversationClient for Agent {
    async fn respond(&self, _: &CallContext, _: &str) -> Result<String, VoiceError> {
        self.started.notify_one();
        tokio::time::sleep(self.delay).await;
        if self.failed {
            return Err(VoiceError::Provider {
                provider: "core",
                message: "401".into(),
            });
        }
        Ok("Chennai is sunny.".into())
    }
}
struct Tts;
#[async_trait]
impl TtsProvider for Tts {
    async fn synthesize(&self, text: &str) -> Result<AudioStream, VoiceError> {
        Ok(Box::pin(futures_util::stream::iter(vec![Ok(
            Bytes::copy_from_slice(text.as_bytes()),
        )])))
    }
}

async fn run(delay: Duration, failed: bool, choice: &'static str) -> Vec<CallCommand> {
    run_scenario(delay, failed, choice, false, false).await
}

async fn run_scenario(
    delay: Duration,
    failed: bool,
    choice: &'static str,
    interrupt: bool,
    greeting: bool,
) -> Vec<CallCommand> {
    let started = Arc::new(Notify::new());
    let observed = started.clone();
    let app = Router::new().route(
        "/jev",
        post(move || {
            let observed = observed.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(1), observed.notified())
                    .await
                    .unwrap();
                Json(serde_json::json!({"answers":{"acknowledgement":{"choice":choice}}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/jev", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let jev =
        Arc::new(JevClient::new(reqwest::Client::new(), "test".into()).with_endpoint(endpoint));
    let playback = Arc::new(PlaybackState::new());
    playback.begin();
    let (output, mut rx) = mpsc::channel(32);
    let (signal, mut done) = mpsc::channel(1);
    let now = Instant::now();
    let answer_started = Arc::new(AtomicBool::new(false));
    let task = spawn_response(
        if greeting { 1 } else { 2 },
        CallContext {
            channel: "phone".into(),
            external_identity: "test".into(),
            external_conversation_id: "test".into(),
            initiation_context: greeting.then(|| "Greet the caller.".into()),
            turn_id: Some("turn".into()),
            revision: Some(0),
            tts_provider: None,
            filler: None,
        },
        "Weather in Chennai?".into(),
        Some(TurnTiming {
            speech_started_at: Some(now),
            last_voiced_audio_at: Some(now),
            transcript_received_at: now,
            last_transcript_received_at: now,
        }),
        Arc::new(Agent {
            delay,
            started,
            failed,
        }),
        Arc::new(Tts),
        Arc::new(Tts),
        Some(jev),
        output,
        signal,
        Arc::new(AtomicBool::new(false)),
        answer_started.clone(),
        playback.clone(),
    );
    if interrupt {
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!answer_started.load(std::sync::atomic::Ordering::SeqCst));
        playback.invalidate();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::sleep(Duration::from_millis(100)).await;
    } else {
        tokio::time::timeout(Duration::from_secs(4), done.recv())
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
    }
    let mut commands = Vec::new();
    while let Ok(command) = rx.try_recv() {
        commands.push(command);
    }
    server.abort();
    commands
}

#[tokio::test]
async fn slow_lookup_acknowledges_while_core_runs_then_answers() {
    let commands = run(Duration::from_millis(1200), false, "11").await;
    let kinds: Vec<_> = commands
        .iter()
        .filter_map(|c| match c {
            CallCommand::Media { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, vec![AudioKind::Filler, AudioKind::Answer]);
    let filler = commands
        .iter()
        .find_map(|command| match command {
            CallCommand::Media {
                kind: AudioKind::Filler,
                queued_at,
                latency_origin_at: Some(origin),
                bytes,
                ..
            } => Some((queued_at.duration_since(*origin), bytes)),
            _ => None,
        })
        .unwrap();
    assert!(filler.0 < Duration::from_millis(1000));
    assert_eq!(filler.1.as_ref(), b"Let me check the weather there.");
}

#[tokio::test]
async fn fast_answer_and_pleasantry_skip_acknowledgement() {
    for (delay, choice) in [
        (Duration::from_millis(20), "11"),
        (Duration::from_millis(1200), "none"),
    ] {
        let commands = run(delay, false, choice).await;
        assert!(!commands.iter().any(|c| matches!(
            c,
            CallCommand::Media {
                kind: AudioKind::Filler,
                ..
            }
        )));
    }
}

#[tokio::test]
async fn rejected_core_request_cancels_pending_acknowledgement() {
    let commands = run(Duration::from_millis(20), true, "11").await;
    assert!(!commands.iter().any(|c| matches!(
        c,
        CallCommand::Media {
            kind: AudioKind::Filler,
            ..
        }
    )));
    assert!(commands.iter().any(|c| matches!(
        c,
        CallCommand::Media {
            kind: AudioKind::Apology,
            ..
        }
    )));
}

#[tokio::test]
async fn greeting_skips_acknowledgement_even_when_slow() {
    let commands = run_scenario(Duration::from_millis(1200), false, "11", false, true).await;
    assert!(!commands.iter().any(|command| matches!(
        command,
        CallCommand::Media {
            kind: AudioKind::Filler,
            ..
        }
    )));
}

#[tokio::test]
async fn interruption_cancels_answer_after_acknowledgement() {
    let commands = run_scenario(Duration::from_millis(1200), false, "11", true, false).await;
    assert!(commands.iter().any(|command| matches!(
        command,
        CallCommand::Media {
            kind: AudioKind::Filler,
            ..
        }
    )));
    assert!(!commands.iter().any(|command| matches!(
        command,
        CallCommand::Media {
            kind: AudioKind::Answer,
            ..
        }
    )));
}

#[tokio::test]
async fn malformed_jev_choice_stays_silent_then_answers() {
    let commands = run(Duration::from_millis(1200), false, "unexpected").await;
    assert!(!commands.iter().any(|command| matches!(command, CallCommand::Media { kind: AudioKind::Filler, .. })));
    assert!(commands.iter().any(|command| matches!(
        command,
        CallCommand::Media {
            kind: AudioKind::Answer,
            ..
        }
    )));
}
