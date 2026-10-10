/**
* this file code contains agent response streaming and tts synthesis
*/
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::channels::context::CallContext;
use crate::core::{ConversationClient, ConversationEvent, ConversationEventStream};
use crate::providers::jev::JevClient;
use crate::providers::tts::TtsProvider;
use crate::voice::chunker::SentenceChunker;
use crate::voice::filler::{
    is_conversational_pleasantry, play_filler, rotate_filler_for_choice_and_tone, strip_leading_ack,
};
use crate::voice::metrics::{ResponseMetrics, TurnTiming, log_turn_latency};
use crate::voice::provider::VoiceError;
use crate::voice::session::mod_types::{AudioKind, CallCommand, SessionSignal};
use crate::voice::session::playback::PlaybackState;

enum Spoken {
    Filler(&'static str),
    Sentence {
        text: String,
        queued_at: std::time::Instant,
    },
    Mark(String),
}

struct ResponseLifecycle {
    number: u64,
    context: CallContext,
    generation: u64,
    kind: &'static str,
    metrics: Arc<Mutex<ResponseMetrics>>,
    outcome: &'static str,
}

impl Drop for ResponseLifecycle {
    fn drop(&mut self) {
        if let Ok(metrics) = self.metrics.lock() {
            log_turn_latency(
                self.number,
                &self.context,
                self.generation,
                self.kind,
                &metrics,
                self.outcome,
                std::time::Instant::now(),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_response(
    number: u64,
    context: CallContext,
    transcript: String,
    timing: Option<TurnTiming>,
    agent: Arc<dyn ConversationClient>,
    tts: Arc<dyn TtsProvider>,
    filler_tts: Arc<dyn TtsProvider>,
    jev: Option<Arc<JevClient>>,
    output: mpsc::Sender<CallCommand>,
    signal: mpsc::Sender<SessionSignal>,
    audio_playing: Arc<std::sync::atomic::AtomicBool>,
    answer_started: Arc<std::sync::atomic::AtomicBool>,
    playback: Arc<PlaybackState>,
) -> JoinHandle<()> {
    let generation = playback.current_generation();
    let kind = if timing
        .as_ref()
        .is_some_and(|t| t.speech_started_at.is_none())
        && context.initiation_context.is_some()
        && number == 1
    {
        "greeting"
    } else {
        "turn"
    };
    let metrics = Arc::new(Mutex::new(ResponseMetrics::new(
        std::time::Instant::now(),
        timing.clone(),
    )));
    let span = tracing::info_span!("voice_response", conversation_id = %context.external_conversation_id,
        turn_id = context.turn_id.as_deref(), revision = context.revision, turn = number, generation, kind);
    let lifecycle = ResponseLifecycle {
        number,
        context: context.clone(),
        generation,
        kind,
        metrics: metrics.clone(),
        outcome: "cancelled",
    };
    tokio::spawn(
        async move {
            let mut lifecycle = lifecycle;
            let apology_tts = tts.clone();
            let apology_output = output.clone();
            let apology_answer_started = answer_started.clone();
            let apology_playback = playback.clone();
            let result = stream_response(
                number,
                &context,
                &transcript,
                metrics.clone(),
                agent,
                tts,
                filler_tts,
                jev,
                output,
                audio_playing.clone(),
                answer_started,
                playback,
            )
            .await;
            let response_text = match &result {
                Ok(text) => {
                    let snapshot = metrics.lock().unwrap();
                    lifecycle.outcome = if snapshot.first_answer_enqueued.is_none() {
                        "no_answer_audio"
                    } else if snapshot.tts_errors > 0 {
                        "degraded"
                    } else {
                        "completed"
                    };
                    text.clone()
                }
                Err(error) => {
                    lifecycle.outcome = "failed";
                    tracing::warn!(error_kind = error.kind(), "VOICE_RESPONSE_FAILED");
                    speak_apology(
                        number,
                        error,
                        &apology_tts,
                        &apology_output,
                        &audio_playing,
                        &apology_answer_started,
                        &apology_playback,
                    )
                    .await;
                    String::new()
                }
            };
            let _ = signal
                .send(SessionSignal::ResponseFinished {
                    number,
                    response_text,
                })
                .await;
        }
        .instrument(span),
    )
}

#[allow(clippy::too_many_arguments)]
async fn stream_response(
    number: u64,
    context: &CallContext,
    transcript: &str,
    metrics: Arc<Mutex<ResponseMetrics>>,
    agent: Arc<dyn ConversationClient>,
    tts: Arc<dyn TtsProvider>,
    filler_tts: Arc<dyn TtsProvider>,
    jev: Option<Arc<JevClient>>,
    output: mpsc::Sender<CallCommand>,
    audio_playing: Arc<std::sync::atomic::AtomicBool>,
    answer_started: Arc<std::sync::atomic::AtomicBool>,
    playback: Arc<PlaybackState>,
) -> Result<String, VoiceError> {
    tracing::info!(
        turn = number,
        conversation_id = %context.external_conversation_id,
        turn_id = ?context.turn_id,
        prompt_len = transcript.len(),
        "Voice pipeline: processing turn"
    );

    let generation = playback.current_generation();
    let (spoken_tx, mut spoken_rx) = mpsc::channel(8);
    let player_output = output.clone();
    let player_tts = tts.clone();
    let player_filler_tts = filler_tts.clone();
    let player_audio = audio_playing.clone();
    let player_answer = answer_started.clone();
    let player_playback = playback.clone();
    let player_metrics = metrics.clone();
    let player_span = tracing::Span::current();
    let response_mark = format!("response-{number}");
    let player = tokio::spawn(
        async move {
            let mut first_tts_ttfb_ms = None;
            let mut first_audio_sent_at = None;
            let mut filler_audio_at = None;
            while let Some(job) = spoken_rx.recv().await {
                if !player_playback.accepts(generation) {
                    break;
                }
                match job {
                    Spoken::Filler(phrase) => {
                        if play_filler(
                            phrase,
                            player_filler_tts.as_ref(),
                            &player_output,
                            &mut filler_audio_at,
                            &player_audio,
                            generation,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Spoken::Sentence {
                        text: sentence,
                        queued_at,
                    } => {
                        tracing::info!(
                            speech_queue_wait_ms = queued_at.elapsed().as_millis() as u64,
                            sentence_bytes = sentence.len(),
                            "VOICE_TTS_SENTENCE_STARTED"
                        );
                        match play_sentence(
                            &sentence,
                            player_tts.as_ref(),
                            &player_output,
                            &mut first_audio_sent_at,
                            &player_audio,
                            &player_answer,
                            &player_playback,
                            generation,
                            AudioKind::Answer,
                            Some(&player_metrics),
                        )
                        .await
                        {
                            Ok(ttfb) => {
                                if first_tts_ttfb_ms.is_none() {
                                    first_tts_ttfb_ms = Some(ttfb);
                                }
                            }
                            Err(err) => {
                                player_metrics.lock().unwrap().tts_errors += 1;
                                tracing::warn!(
                                    error_kind = err.kind(),
                                    sentence_bytes = sentence.len(),
                                    "VOICE_TTS_SENTENCE_FAILED"
                                );
                            }
                        }
                    }
                    Spoken::Mark(name) => {
                        player_metrics.lock().unwrap().first_filler_enqueued = filler_audio_at;
                        if player_output
                            .send(CallCommand::Mark { name, generation })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            if player_playback.accepts(generation)
                && (first_audio_sent_at.is_some() || filler_audio_at.is_some())
            {
                let _ = player_output
                    .send(CallCommand::Mark {
                        name: response_mark,
                        generation,
                    })
                    .await;
            }
            (first_tts_ttfb_ms, first_audio_sent_at, filler_audio_at)
        }
        .instrument(player_span),
    );

    let mut chunker = SentenceChunker::new();
    let mut full_response = String::new();
    let mut sentence_count = 0usize;
    let filler_decision_started = std::time::Instant::now();

    let mut filler_played = false;
    let is_opening = number == 1 || transcript == "The call just connected. Greet the user.";
    let is_pleasantry = is_conversational_pleasantry(transcript);
    let mut mut_context = context.clone();

    if !is_opening && !is_pleasantry {
        let (choice, tone) = if let Some(client) = &jev {
            match tokio::time::timeout(
                Duration::from_millis(250),
                client.choose_filler_and_tone(transcript),
            )
            .await
            {
                Ok(Ok((c, t))) => (c, t),
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "Jev filler choice failed, using fallback");
                    ("looking_into_that".to_string(), "calm".to_string())
                }
                Err(_) => {
                    tracing::warn!("Jev filler choice timed out, using fallback");
                    ("looking_into_that".to_string(), "calm".to_string())
                }
            }
        } else {
            ("looking_into_that".to_string(), "calm".to_string())
        };

        let filler_phrase = rotate_filler_for_choice_and_tone(&choice, &tone);
        if !filler_phrase.is_empty() {
            filler_played = true;
            mut_context.filler = Some(filler_phrase.to_string());
            spoken_tx
                .send(Spoken::Filler(filler_phrase))
                .await
                .map_err(|_| VoiceError::Protocol("speech output closed".into()))?;
            spoken_tx
                .send(Spoken::Mark(format!("filler-{number}")))
                .await
                .map_err(|_| VoiceError::Protocol("speech output closed".into()))?;
            tracing::info!(
                turn = number,
                phrase = filler_phrase,
                choice = %choice,
                tone = %tone,
                "VOICE_LOOKUP_ACKNOWLEDGED"
            );
        }
    }

    {
        let mut m = metrics.lock().unwrap();
        m.filler_decision_ms = Some(filler_decision_started.elapsed().as_millis() as u64);
        m.core_requested = Some(std::time::Instant::now());
        m.last_stage = "core_headers";
    }
    let core_request_start = std::time::Instant::now();
    let mut connect_future = Box::pin(tokio::time::timeout(
        Duration::from_secs(30),
        agent.respond_events(&mut_context, transcript),
    ));
    let mut text_stream: Option<ConversationEventStream> = None;

    loop {
        let next = tokio::select! {
            biased;
            res = &mut connect_future, if text_stream.is_none() => {
                let stream_res = res.map_err(|_| VoiceError::Timeout("Core response"))?;
                text_stream = Some(stream_res?);
                { let mut m = metrics.lock().unwrap(); m.core_opened = Some(std::time::Instant::now()); m.last_stage = "core_stream"; }
                tracing::info!(elapsed_ms = core_request_start.elapsed().as_millis() as u64, "VOICE_CORE_STREAM_OPENED");
                continue;
            }
            result = async {
                match text_stream.as_mut() {
                    Some(s) => tokio::time::timeout(Duration::from_secs(30), s.next()).await,
                    None => std::future::pending().await,
                }
            }, if text_stream.is_some() => {
                result.map_err(|_| VoiceError::Timeout("Core stream"))?
            }
        };
        let Some(event) = next else {
            break;
        };
        let ConversationEvent::Text(chunk) = event?;
        if chunk.trim().is_empty() {
            continue;
        }
        {
            let mut m = metrics.lock().unwrap();
            m.text_chunks += 1;
            m.reply_bytes += chunk.len() as u64;
        }
        if metrics.lock().unwrap().first_text.is_none() {
            metrics.lock().unwrap().first_text = Some(std::time::Instant::now());
            tracing::info!(
                turn = number,
                conversation_id = %context.external_conversation_id,
                turn_id = ?context.turn_id,
                stage = "core_first_text",
                since_request_ms = core_request_start.elapsed().as_millis(),
                "VOICE_MILESTONE"
            );
        }
        full_response.push_str(&chunk);

        let sentences = chunker.push(&chunk);
        for sentence in sentences {
            let mut trimmed = sentence.trim();
            if trimmed.is_empty() {
                continue;
            }
            if filler_played {
                trimmed = strip_leading_ack(trimmed);
                if trimmed.is_empty() {
                    continue;
                }
            }
            sentence_count += 1;
            metrics.lock().unwrap().sentences = sentence_count as u64;
            if sentence_count == 1 {
                metrics.lock().unwrap().first_sentence = Some(std::time::Instant::now());
                tracing::info!(
                    turn = number,
                    conversation_id = %context.external_conversation_id,
                    turn_id = ?context.turn_id,
                    stage = "first_sentence",
                    since_request_ms = core_request_start.elapsed().as_millis(),
                    sentence_chars = trimmed.len(),
                    "VOICE_MILESTONE"
                );
            }
            spoken_tx
                .send(Spoken::Sentence {
                    text: trimmed.to_string(),
                    queued_at: std::time::Instant::now(),
                })
                .await
                .map_err(|_| VoiceError::Protocol("speech output closed".into()))?;
        }
    }

    if let Some(remaining) = chunker.flush() {
        let mut trimmed = remaining.trim();
        if !trimmed.is_empty() {
            if filler_played {
                trimmed = strip_leading_ack(trimmed);
            }
            if !trimmed.is_empty() {
                sentence_count += 1;
                metrics.lock().unwrap().sentences = sentence_count as u64;
                if sentence_count == 1 {
                    metrics.lock().unwrap().first_sentence = Some(std::time::Instant::now());
                }
                spoken_tx
                    .send(Spoken::Sentence {
                        text: trimmed.to_string(),
                        queued_at: std::time::Instant::now(),
                    })
                    .await
                    .map_err(|_| VoiceError::Protocol("speech output closed".into()))?;
            }
        }
    }

    {
        let mut m = metrics.lock().unwrap();
        m.text_finished = Some(std::time::Instant::now());
        m.sentences = sentence_count as u64;
        m.last_stage = "tts_drain";
    }
    drop(spoken_tx);
    let _ = player.await;
    metrics.lock().unwrap().last_stage = "generation_finished";

    Ok(full_response)
}

/// Speaks a short apology when a turn fails outright, so the caller never gets dead air.
/// Reuses `play_sentence`'s TTS/output/mark plumbing rather than a bespoke path.
#[allow(clippy::too_many_arguments)]
async fn speak_apology(
    number: u64,
    error: &VoiceError,
    tts: &Arc<dyn TtsProvider>,
    output: &mpsc::Sender<CallCommand>,
    audio_playing: &Arc<std::sync::atomic::AtomicBool>,
    answer_started: &Arc<std::sync::atomic::AtomicBool>,
    playback: &Arc<PlaybackState>,
) {
    let generation = playback.current_generation();
    if !playback.accepts(generation) {
        audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
        return;
    }
    let apology = match error {
        VoiceError::Timeout(_) => {
            "Sorry, that's taking longer than expected. Could you say that again?"
        }
        _ => "Sorry, I ran into a problem with that. Could you try again?",
    };
    let mut first_audio_tracker = None;
    let played = play_sentence(
        apology,
        tts.as_ref(),
        output,
        &mut first_audio_tracker,
        audio_playing,
        answer_started,
        playback,
        generation,
        AudioKind::Apology,
        None,
    )
    .await
    .is_ok();
    if !played {
        audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
        return;
    }
    let _ = output
        .send(CallCommand::Mark {
            name: format!("response-{number}"),
            generation,
        })
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn play_sentence(
    sentence: &str,
    tts: &dyn TtsProvider,
    output: &mpsc::Sender<CallCommand>,
    first_audio_tracker: &mut Option<std::time::Instant>,
    audio_playing: &std::sync::atomic::AtomicBool,
    answer_started: &std::sync::atomic::AtomicBool,
    playback: &PlaybackState,
    generation: u64,
    kind: AudioKind,
    metrics: Option<&Arc<Mutex<ResponseMetrics>>>,
) -> Result<u128, VoiceError> {
    let text = sentence.trim();
    if text.is_empty() || !text.chars().any(|c| c.is_alphabetic()) {
        return Ok(0);
    }
    let tts_start = std::time::Instant::now();
    let mut report = TtsReport::new(kind, text.len());
    let mut audio = tts.synthesize(text).await.inspect_err(|_| {
        report.outcome = "failed";
    })?;
    report.headers_ms = Some(report.started.elapsed().as_millis() as u64);
    let mut ttfb_ms = 0;
    let mut is_first_chunk = true;

    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(10), audio.next())
        .await
        .map_err(|_| {
            report.outcome = "timeout";
            VoiceError::Timeout("TTS audio")
        })?
    {
        if !playback.accepts(generation) {
            break;
        }
        let chunk_bytes = chunk.inspect_err(|_| {
            report.outcome = "failed";
        })?;
        if chunk_bytes.is_empty() {
            continue;
        }
        let received = std::time::Instant::now();
        report.record(chunk_bytes.len(), received);
        if is_first_chunk && let Some(metrics) = metrics {
            metrics
                .lock()
                .unwrap()
                .first_tts_ttfb_ms
                .get_or_insert(tts_start.elapsed().as_millis() as u64);
        }
        let queued_at = std::time::Instant::now();
        output
            .send(CallCommand::Media {
                bytes: chunk_bytes,
                generation,
                kind,
                queued_at,
                latency_origin_at: metrics.map(|metrics| {
                    let m = metrics.lock().unwrap();
                    m.timing
                        .as_ref()
                        .map(|t| t.transcript_received_at)
                        .unwrap_or(m.started)
                }),
            })
            .await
            .map_err(|_| VoiceError::Protocol("call output closed".into()))?;

        answer_started.store(true, std::sync::atomic::Ordering::SeqCst);
        audio_playing.store(true, std::sync::atomic::Ordering::SeqCst);

        let now = std::time::Instant::now();
        if is_first_chunk {
            ttfb_ms = received.duration_since(tts_start).as_millis();
            if let Some(metrics) = metrics {
                metrics
                    .lock()
                    .unwrap()
                    .first_answer_enqueued
                    .get_or_insert(now);
            }
            tracing::info!(
                stage = "audio_enqueued",
                audio_kind = kind.label(),
                tts_ttfb_ms = ttfb_ms,
                sentence_chars = text.len(),
                "VOICE_MILESTONE"
            );
            if first_audio_tracker.is_none() {
                *first_audio_tracker = Some(now);
            }
            is_first_chunk = false;
        }
    }
    report.outcome = if !playback.accepts(generation) {
        "invalidated"
    } else if report.frames == 0 {
        "empty"
    } else {
        "completed"
    };
    Ok(ttfb_ms)
}

struct TtsReport {
    started: std::time::Instant,
    kind: AudioKind,
    text_bytes: usize,
    headers_ms: Option<u64>,
    first_audio_ms: Option<u64>,
    last_audio: Option<std::time::Instant>,
    max_gap_ms: u64,
    bytes: u64,
    frames: u64,
    outcome: &'static str,
}
impl TtsReport {
    fn new(kind: AudioKind, text_bytes: usize) -> Self {
        Self {
            started: std::time::Instant::now(),
            kind,
            text_bytes,
            headers_ms: None,
            first_audio_ms: None,
            last_audio: None,
            max_gap_ms: 0,
            bytes: 0,
            frames: 0,
            outcome: "cancelled",
        }
    }
    fn record(&mut self, bytes: usize, now: std::time::Instant) {
        self.first_audio_ms
            .get_or_insert(now.duration_since(self.started).as_millis() as u64);
        if let Some(last) = self.last_audio {
            self.max_gap_ms = self
                .max_gap_ms
                .max(now.duration_since(last).as_millis() as u64);
        }
        self.last_audio = Some(now);
        self.bytes += bytes as u64;
        self.frames += 1;
    }
}
impl Drop for TtsReport {
    fn drop(&mut self) {
        tracing::info!(
            audio_kind = self.kind.label(),
            outcome = self.outcome,
            text_bytes = self.text_bytes,
            headers_ms = self.headers_ms,
            first_audio_ms = self.first_audio_ms,
            max_audio_gap_ms = self.max_gap_ms,
            audio_bytes = self.bytes,
            audio_frames = self.frames,
            elapsed_ms = self.started.elapsed().as_millis() as u64,
            "VOICE_TTS_SENTENCE"
        );
    }
}
