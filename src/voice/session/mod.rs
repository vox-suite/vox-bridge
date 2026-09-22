// this file code contains voice session orchestrator and event loop

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
use crate::voice::filler::prewarm_fillers;
use crate::voice::metrics::TurnTiming;
use crate::voice::provider::{VoiceError, VoiceProviders};
use crate::voice::session::response::spawn_response;
use crate::voice::session::speculation::spawn_speculation_watcher;
use crate::voice::turn::DraftTurn;

pub async fn run_voice_session(
    providers: VoiceProviders,
    mut context: CallContext,
    mut input: mpsc::Receiver<CallEvent>,
    output: mpsc::Sender<CallCommand>,
) -> Result<(), VoiceError> {
    prewarm_fillers(providers.tts.clone());
    let stt = providers.stt.connect().await?;

    let (signal_tx, mut signal_rx) = mpsc::channel(64);
    let (speculation_tx, speculation_rx) = watch::channel(None);
    let _speculation_watcher =
        spawn_speculation_watcher(speculation_rx, providers.agent.clone());

    let playback = Arc::new(PlaybackState::new());
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
            output.clone(),
            signal_tx.clone(),
            audio_playing.clone(),
            answer_started.clone(),
        ));
    }

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
                match stt_event {
                    Ok(Some(SttEvent::SpeechStarted)) => {
                        speech_started_at.get_or_insert_with(std::time::Instant::now);
                        settle_at = None;
                        let is_playing = audio_playing.swap(false, std::sync::atomic::Ordering::SeqCst);
                        if is_playing || active_response.is_some() {
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
                    Ok(Some(SttEvent::PartialTranscript(text))) => {
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
                        let now = std::time::Instant::now();
                        let timing = draft_timing.get_or_insert(TurnTiming {
                            speech_started_at: speech_started_at.take(),
                            last_audio_at,
                            transcript_received_at: now,
                        });
                        timing.last_audio_at = last_audio_at;
                        if let Some(task) = active_response.take() {
                            task.abort();
                            let _ = task.await;
                            if !answer_started.load(std::sync::atomic::Ordering::SeqCst)
                                && draft.text.is_empty()
                                && let Some((previous, previous_timing)) = dispatched.take()
                            {
                                draft = previous;
                                draft_timing = Some(TurnTiming {
                                    last_audio_at,
                                    ..previous_timing
                                });
                            }
                        }
                        if audio_playing.swap(false, std::sync::atomic::Ordering::SeqCst) {
                            playback.invalidate();
                            output
                                .send(CallCommand::Clear)
                                .await
                                .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
                            dispatched = None;
                        }
                        draft.finish(&text);
                        settle_at = Some(tokio::time::Instant::now() + draft.settle_delay());
                        let mut ctx = context.clone();
                        ctx.turn_id = Some(draft.id.clone());
                        ctx.revision = Some(draft.revision);
                        if draft.snapshot().split_whitespace().count() >= 3 {
                            let _ = speculation_tx.send(Some((ctx, draft.snapshot())));
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, "STT stream error");
                    }
                }
            }
            _ = async {
                match settle_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                settle_at = None;
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
                context.voice_signature = None;
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
                    output.clone(),
                    signal_tx.clone(),
                    audio_playing.clone(),
                    answer_started.clone(),
                ));
            }
            signal = signal_rx.recv() => {
                match signal {
                    Some(SessionSignal::ResponseFinished(number)) => {
                        if number == response_number {
                            active_response.take();
                            if !audio_playing.load(std::sync::atomic::Ordering::SeqCst) {
                                dispatched = None;
                            }
                        }
                    }
                    None => break,
                }
            }
        }
    }

    if let Some(task) = active_response.take() {
        task.abort();
    }
    let _ = stt.finish().await;
    Ok(())
}
