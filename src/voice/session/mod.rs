/**
* this file code contains voice session orchestrator and event loop
*/
pub mod mod_types;
pub mod playback;
pub mod response;
pub mod speculation;

pub use mod_types::{CallCommand, CallEvent, SessionSignal};
pub use playback::PlaybackState;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::channels::context::CallContext;
use crate::providers::stt::SttEvent;
use crate::voice::metrics::TurnTiming;
use crate::voice::provider::{VoiceError, VoiceProviders};
use crate::voice::session::response::spawn_response;
use crate::voice::session::speculation::spawn_speculation_watcher;
use crate::voice::turn::{DraftTurn, is_backchannel};

const MAX_STT_RECONNECTS: u32 = 3;

/// A spawned voice session together with the channels used to drive it.
/// Shared by every channel (Twilio, desktop, ...) that bridges a transport
/// socket onto `run_voice_session_with_playback`.
pub struct VoiceSessionHandle {
    pub playback: Arc<PlaybackState>,
    pub input: mpsc::Sender<CallEvent>,
    pub output: mpsc::Receiver<CallCommand>,
    pub task: JoinHandle<Result<(), VoiceError>>,
}

pub fn spawn_playback_session(
    providers: VoiceProviders,
    context: CallContext,
) -> VoiceSessionHandle {
    let playback = Arc::new(PlaybackState::new());
    let (input_tx, input_rx) = mpsc::channel(64);
    let (output_tx, output_rx) = mpsc::channel(64);
    let task = tokio::spawn(run_voice_session_with_playback(
        providers,
        context,
        input_rx,
        output_tx,
        playback.clone(),
    ));
    VoiceSessionHandle {
        playback,
        input: input_tx,
        output: output_rx,
        task,
    }
}

/// Waits briefly for the session task to wind down on its own, then aborts it,
/// and closes out the conversation with the agent either way.
pub async fn shutdown_playback_session(
    mut task: JoinHandle<Result<(), VoiceError>>,
    providers: &VoiceProviders,
    context: &CallContext,
) {
    if !task.is_finished()
        && tokio::time::timeout(std::time::Duration::from_secs(5), &mut task)
            .await
            .is_err()
    {
        task.abort();
    }
    let _ = providers.agent.complete(context).await;
}

pub async fn run_voice_session(
    providers: VoiceProviders,
    context: CallContext,
    input: mpsc::Receiver<CallEvent>,
    output: mpsc::Sender<CallCommand>,
) -> Result<(), VoiceError> {
    run_voice_session_with_playback(
        providers,
        context,
        input,
        output,
        Arc::new(PlaybackState::new()),
    )
    .await
}

pub async fn run_voice_session_with_playback(
    providers: VoiceProviders,
    mut context: CallContext,
    mut input: mpsc::Receiver<CallEvent>,
    output: mpsc::Sender<CallCommand>,
    playback: Arc<PlaybackState>,
) -> Result<(), VoiceError> {
    let stt_connect = providers.stt.connect();

    let (signal_tx, mut signal_rx) = mpsc::channel(64);
    let (speculation_tx, speculation_rx) = watch::channel(None);
    let _speculation_watcher = spawn_speculation_watcher(speculation_rx, providers.agent.clone());

    let audio_playing = Arc::new(AtomicBool::new(false));
    let answer_started = Arc::new(AtomicBool::new(false));

    let mut response_number = 0u64;
    let mut draft = DraftTurn::default();
    let mut draft_timing: Option<TurnTiming> = None;
    let mut active_response: Option<JoinHandle<()>> = None;
    let mut settle_at: Option<tokio::time::Instant> = None;
    let mut dispatched: Option<(DraftTurn, TurnTiming)> = None;

    let mut speech_started_at: Option<std::time::Instant> = None;
    let mut last_audio_at: Option<std::time::Instant> = None;
    let mut completeness_rx: Option<mpsc::Receiver<f64>> = None;
    let mut settle_origin: Option<tokio::time::Instant> = None;

    if let Some(instruction) = context.initiation_context.clone() {
        response_number += 1;
        playback.begin();
        audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
        answer_started.store(false, std::sync::atomic::Ordering::SeqCst);
        let timing = TurnTiming {
            speech_started_at: None,
            last_audio_at: None,
            transcript_received_at: std::time::Instant::now(),
        };
        dispatched = Some((DraftTurn::default(), timing.clone()));
        active_response = Some(spawn_response(
            response_number,
            context.clone(),
            instruction,
            Some(timing),
            providers.agent.clone(),
            providers.tts.clone(),
            providers.filler_tts.clone(),
            providers.jev.clone(),
            output.clone(),
            signal_tx.clone(),
            audio_playing.clone(),
            answer_started.clone(),
            playback.clone(),
        ));
    }

    let mut stt = stt_connect.await?;
    let mut stt_reconnects = 0u32;

    loop {
        tokio::select! {
            event = input.recv() => {
                match event {
                    Some(CallEvent::Audio(bytes)) => {
                        last_audio_at = Some(std::time::Instant::now());
                        if let Err(err) = stt.send_audio(bytes).await {
                            tracing::warn!(error = %err, "STT send audio failed");
                        }
                    }
                    Some(CallEvent::PlaybackFinished(name)) => {
                        if name == format!("filler-{response_number}")
                            && !answer_started.load(std::sync::atomic::Ordering::SeqCst)
                        {
                            audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
                            playback.set_playing(false);
                        }
                        if name == format!("response-{response_number}") {
                            audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
                            playback.set_playing(false);
                            dispatched = None;
                            tracing::info!(turn = response_number, conversation_id = %context.external_conversation_id, "VOICE_PLAYBACK_ACKNOWLEDGED");
                        }
                    }
                    Some(CallEvent::Stop) | None => break,
                }
            }
            stt_event = stt.next_event() => {
                if matches!(stt_event, Ok(Some(_))) {
                    stt_reconnects = 0;
                }
                match stt_event {
                    Ok(Some(SttEvent::SpeechStarted)) => {
                        speech_started_at.get_or_insert_with(std::time::Instant::now);
                        settle_at = None;
                    }
                    Ok(Some(SttEvent::PartialTranscript(text))) => {
                        let assistant_active = audio_playing.load(std::sync::atomic::Ordering::SeqCst)
                            || active_response.is_some();
                        if assistant_active {
                            let is_ack = is_backchannel(&text);

                            if is_ack {
                                tracing::info!(text = %text, "VOICE_BACKCHANNEL_IGNORED");
                            } else {
                                let is_playing = audio_playing.swap(false, std::sync::atomic::Ordering::SeqCst);
                                playback.invalidate();
                                if let Some(task) = active_response.take() {
                                    task.abort();
                                    let _ = task.await;
                                    if !answer_started.load(std::sync::atomic::Ordering::SeqCst)
                                        && draft.text.is_empty()
                                        && let Some((previous, timing)) = dispatched.take()
                                    {
                                        draft = previous;
                                        draft_timing = Some(timing);
                                    }
                                }
                                if answer_started.load(std::sync::atomic::Ordering::SeqCst) {
                                    dispatched = None;
                                }
                                output
                                    .send(CallCommand::Clear)
                                    .await
                                    .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
                                tracing::info!(turn = response_number, playing = is_playing, "VOICE_TURN_INTERRUPTED");
                            }
                        }

                        if draft.partial(&text) && draft.snapshot().split_whitespace().count() >= 3 {
                            let mut ctx = context.clone();
                            ctx.turn_id = Some(draft.id.clone());
                            ctx.revision = Some(draft.revision);
                            let _ = speculation_tx.send(Some((ctx, draft.snapshot())));
                        }
                        settle_at = None;
                    }
                    Ok(Some(SttEvent::FinalTranscript(text))) => {
                        if text.trim().is_empty() {
                            continue;
                        }

                        let assistant_active = audio_playing.load(std::sync::atomic::Ordering::SeqCst)
                            || active_response.is_some();
                        if assistant_active && is_backchannel(&text) {
                            tracing::info!(text = %text, "VOICE_BACKCHANNEL_IGNORED");
                            speech_started_at = None;
                            continue;
                        }

                        if assistant_active {
                            let is_playing = audio_playing.swap(false, std::sync::atomic::Ordering::SeqCst);
                            playback.invalidate();
                            if let Some(task) = active_response.take() {
                                task.abort();
                                let _ = task.await;
                            }
                            dispatched = None;
                            output
                                .send(CallCommand::Clear)
                                .await
                                .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
                            tracing::info!(turn = response_number, playing = is_playing, "VOICE_TURN_INTERRUPTED");
                        }

                        let now = std::time::Instant::now();
                        let timing = draft_timing.get_or_insert(TurnTiming {
                            speech_started_at: speech_started_at.take(),
                            last_audio_at,
                            transcript_received_at: now,
                        });
                        timing.last_audio_at = last_audio_at;

                        draft.finish(&text);
                        let origin = tokio::time::Instant::now();
                        settle_origin = Some(origin);
                        settle_at = Some(origin + draft.settle_delay());
                        if let Some(jev) = &providers.jev {
                            let utterance = text.clone();
                            let jev = jev.clone();
                            let (tx, rx) = mpsc::channel(1);
                            tokio::spawn(async move {
                                if let Ok(score) = jev.is_complete_thought(&utterance).await {
                                    let _ = tx.send(score).await;
                                }
                            });
                            completeness_rx = Some(rx);
                        }
                        let mut ctx = context.clone();
                        ctx.turn_id = Some(draft.id.clone());
                        ctx.revision = Some(draft.revision);
                        let _ = speculation_tx.send(Some((ctx, draft.snapshot())));
                    }
                    // The STT stream ended or failed: reconnect instead of spinning
                    // on a dead socket, and give up after a few attempts.
                    Ok(None) | Err(_) => {
                        if let Err(err) = &stt_event {
                            tracing::warn!(error = %err, "STT stream error");
                        }
                        stt_reconnects += 1;
                        if stt_reconnects > MAX_STT_RECONNECTS {
                            return Err(VoiceError::Protocol("speech recognition unavailable".into()));
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(250 * u64::from(stt_reconnects))).await;
                        stt = providers.stt.connect().await?;
                        tracing::info!(attempt = stt_reconnects, "STT stream reconnected");
                    }
                }
            }
            Some(score) = async {
                match &mut completeness_rx {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                completeness_rx = None;
                if let Some(origin) = settle_origin {
                    let adjusted = draft.settle_delay_for_completeness(Some(score));
                    let target = origin + adjusted;
                    let now = tokio::time::Instant::now();
                    settle_at = Some(if target > now { target } else { now });
                }
            }
            _ = async {
                match settle_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                settle_at = None;
                settle_origin = None;
                completeness_rx = None;
                if draft.text.is_empty() {
                    continue;
                }
                let turn = std::mem::take(&mut draft);
                let timing = draft_timing.take().unwrap_or(TurnTiming {
                    speech_started_at: None,
                    last_audio_at: None,
                    transcript_received_at: std::time::Instant::now(),
                });
                context.turn_id = Some(turn.id.clone());
                context.revision = Some(turn.revision);
                if let Some(task) = active_response.take() {
                    task.abort();
                    let _ = task.await;
                }

                response_number += 1;
                playback.begin();
                answer_started.store(false, std::sync::atomic::Ordering::SeqCst);
                dispatched = Some((turn.clone(), timing.clone()));
                active_response = Some(spawn_response(
                    response_number,
                    context.clone(),
                    turn.text,
                    Some(timing),
                    providers.agent.clone(),
                    providers.tts.clone(),
                    providers.filler_tts.clone(),
                    providers.jev.clone(),
                    output.clone(),
                    signal_tx.clone(),
                    audio_playing.clone(),
                    answer_started.clone(),
                    playback.clone(),
                ));
            }
            signal = signal_rx.recv() => {
                match signal {
                    Some(SessionSignal::ResponseFinished { number, .. })
                        if number == response_number =>
                    {
                        active_response.take();
                        if !audio_playing.load(std::sync::atomic::Ordering::SeqCst) {
                            dispatched = None;
                        }
                    }
                    Some(_) | None => {}
                }
            }
        }
    }

    if let Some(task) = active_response.take() {
        task.abort();
        let _ = task.await;
    }

    Ok(())
}
