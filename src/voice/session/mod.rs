/**
* this file code contains voice session orchestrator and event loop
*/
pub mod mod_types;
pub mod playback;
pub mod response;

pub use mod_types::{AudioKind, CallCommand, CallEvent, SessionSignal};
pub use playback::PlaybackState;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::channels::context::CallContext;
use crate::providers::stt::SttEvent;
use crate::voice::metrics::TurnTiming;
use crate::voice::provider::{VoiceError, VoiceProviders};
use crate::voice::session::response::spawn_response;
use crate::voice::turn::{DraftTurn, is_backchannel};

const MAX_STT_RECONNECTS: u32 = 3;

/// A spawned voice session together with the channels used to drive it.
/// Shared by every channel (Twilio) that bridges a transport
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
    let span =
        tracing::info_span!("voice_session", conversation_id = %context.external_conversation_id);
    let task = tokio::spawn(
        run_voice_session_with_playback(providers, context, input_rx, output_tx, playback.clone())
            .instrument(span),
    );
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
    let session_started = std::time::Instant::now();
    let mut report = SessionReport::new(session_started);
    tracing::info!("VOICE_SESSION_STARTED");
    let stt_connect = providers.stt.connect();

    let (signal_tx, mut signal_rx) = mpsc::channel(64);

    let audio_playing = Arc::new(AtomicBool::new(false));
    let answer_started = Arc::new(AtomicBool::new(false));

    let mut response_number = 0u64;
    let mut draft = DraftTurn::default();
    let mut draft_timing: Option<TurnTiming> = None;
    let mut active_response: Option<JoinHandle<()>> = None;
    let mut settle_at: Option<tokio::time::Instant> = None;
    let mut dispatched: Option<(DraftTurn, TurnTiming)> = None;

    let mut speech_started_at: Option<std::time::Instant> = None;
    let mut last_voiced_audio_at: Option<std::time::Instant> = None;
    let mut completeness_rx: Option<mpsc::Receiver<f64>> = None;
    let mut settle_origin: Option<tokio::time::Instant> = None;

    if let Some(instruction) = context.initiation_context.clone() {
        context.turn_id = Some(uuid::Uuid::new_v4().to_string());
        context.revision = Some(0);
        tracing::info!(
            session_elapsed_ms = session_started.elapsed().as_millis() as u64,
            "VOICE_GREETING_DISPATCHED"
        );
        response_number += 1;
        playback.begin();
        audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
        answer_started.store(false, std::sync::atomic::Ordering::SeqCst);
        let timing = TurnTiming {
            speech_started_at: None,
            last_voiced_audio_at: None,
            transcript_received_at: std::time::Instant::now(),
            last_transcript_received_at: std::time::Instant::now(),
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

    let stt_started = std::time::Instant::now();
    let mut stt = stt_connect.await.inspect_err(|error| {
        report.outcome = "failed";
        tracing::warn!(
            error_kind = error.kind(),
            elapsed_ms = stt_started.elapsed().as_millis() as u64,
            "VOICE_STT_CONNECT_FAILED"
        )
    })?;
    tracing::info!(
        connect_ms = stt_started.elapsed().as_millis() as u64,
        session_elapsed_ms = session_started.elapsed().as_millis() as u64,
        "VOICE_STT_CONNECTED"
    );
    let mut stt_reconnects = 0u32;

    loop {
        tokio::select! {
            event = input.recv() => {
                match event {
                    Some(CallEvent::Audio(bytes)) => {
                        report.input_frames += 1; report.input_bytes += bytes.len() as u64;
                        if crate::voice::audio::levels::has_voice_energy(&bytes) {
                            last_voiced_audio_at = Some(std::time::Instant::now());
                        }
                        let send_started = std::time::Instant::now();
                        if let Err(err) = stt.send_audio(bytes).await {
                            report.stt_errors += 1;
                            tracing::warn!(error_kind = err.kind(), "VOICE_STT_SEND_FAILED");
                        }
                        if send_started.elapsed().as_millis() > 100 {
                            tracing::warn!(send_ms = send_started.elapsed().as_millis() as u64, "VOICE_STT_SEND_STALLED");
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
                        report.partials += 1;
                        let assistant_active = audio_playing.load(std::sync::atomic::Ordering::SeqCst)
                            || active_response.is_some();
                        if assistant_active {
                            let is_ack = is_backchannel(&text);

                            if is_ack {
                                report.backchannels += 1;
                            tracing::info!(transcript_bytes = text.len(), "VOICE_BACKCHANNEL_IGNORED");
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
                                report.interruptions += 1;
                            tracing::info!(turn = response_number, playing = is_playing, "VOICE_TURN_INTERRUPTED");
                            }
                        }

                        draft.partial(&text);
                        settle_at = None;
                    }
                    Ok(Some(SttEvent::FinalTranscript(text))) => {
                        report.finals += 1;
                        if text.trim().is_empty() {
                            continue;
                        }

                        let assistant_active = audio_playing.load(std::sync::atomic::Ordering::SeqCst)
                            || active_response.is_some();
                        if assistant_active && is_backchannel(&text) {
                            report.backchannels += 1;
                            tracing::info!(transcript_bytes = text.len(), "VOICE_BACKCHANNEL_IGNORED");
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
                            report.interruptions += 1;
                            tracing::info!(turn = response_number, playing = is_playing, "VOICE_TURN_INTERRUPTED");
                        }

                        let now = std::time::Instant::now();
                        let timing = draft_timing.get_or_insert(TurnTiming {
                            speech_started_at: speech_started_at.take(),
                            last_voiced_audio_at,
                            transcript_received_at: now,
                            last_transcript_received_at: now,
                        });
                        timing.last_voiced_audio_at = last_voiced_audio_at;
                        timing.last_transcript_received_at = now;
                        draft.finish(&text);
                        tracing::info!(turn_id = %draft.id, revision = draft.revision, transcript_bytes = text.len(), speech_to_final_ms = last_voiced_audio_at.and_then(|at| now.checked_duration_since(at)).map(|d| d.as_millis() as u64), "VOICE_STT_FINAL");
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
                    }
                    // The STT stream ended or failed: reconnect instead of spinning
                    // on a dead socket, and give up after a few attempts.
                    Ok(None) | Err(_) => {
                        if let Err(err) = &stt_event {
                            tracing::warn!(error_kind = err.kind(), "VOICE_STT_STREAM_FAILED");
                        }
                        stt_reconnects += 1;
                        if stt_reconnects > MAX_STT_RECONNECTS {
                            report.outcome = "failed";
                            return Err(VoiceError::Protocol("speech recognition unavailable".into()));
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(250 * u64::from(stt_reconnects))).await;
                        report.stt_reconnects += 1;
                        let reconnect_started = std::time::Instant::now();
                        stt = providers.stt.connect().await?;
                        tracing::info!(attempt = stt_reconnects, connect_ms = reconnect_started.elapsed().as_millis() as u64, "VOICE_STT_RECONNECTED");
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
                    tracing::info!(decision_ms = origin.elapsed().as_millis() as u64, settle_ms = adjusted.as_millis() as u64, "VOICE_COMPLETENESS_DECISION");
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
                    last_voiced_audio_at: None,
                    transcript_received_at: std::time::Instant::now(),
            last_transcript_received_at: std::time::Instant::now(),
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

    report.outcome = "completed";
    tracing::info!(
        session_ms = session_started.elapsed().as_millis() as u64,
        responses = response_number,
        "VOICE_SESSION_ENDED"
    );
    Ok(())
}

struct SessionReport {
    started: std::time::Instant,
    outcome: &'static str,
    input_frames: u64,
    input_bytes: u64,
    partials: u64,
    finals: u64,
    backchannels: u64,
    interruptions: u64,
    stt_errors: u64,
    stt_reconnects: u64,
}
impl SessionReport {
    fn new(started: std::time::Instant) -> Self {
        Self {
            started,
            outcome: "unexpected_exit",
            input_frames: 0,
            input_bytes: 0,
            partials: 0,
            finals: 0,
            backchannels: 0,
            interruptions: 0,
            stt_errors: 0,
            stt_reconnects: 0,
        }
    }
}
impl Drop for SessionReport {
    fn drop(&mut self) {
        tracing::info!(
            outcome = self.outcome,
            session_ms = self.started.elapsed().as_millis() as u64,
            input_frames = self.input_frames,
            input_bytes = self.input_bytes,
            partials = self.partials,
            finals = self.finals,
            backchannels = self.backchannels,
            interruptions = self.interruptions,
            stt_errors = self.stt_errors,
            stt_reconnects = self.stt_reconnects,
            "VOICE_SESSION_SUMMARY"
        );
    }
}
