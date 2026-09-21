use crate::voice::{
    context::CallContext,
    provider::{SttEvent, VoiceError},
    registry::ProviderSet,
};
use bytes::Bytes;
use futures_util::StreamExt;
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEvent {
    Audio(Bytes),
    PlaybackFinished(String),
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallCommand {
    Media(Bytes),
    Mark(String),
    Clear,
}

#[derive(Debug, Clone)]
pub struct TurnTiming {
    pub speech_started_at: Option<std::time::Instant>,
    pub last_audio_at: Option<std::time::Instant>,
    pub transcript_received_at: std::time::Instant,
}

enum SessionSignal {
    Stt(SttEvent),
    SpeechDetected,
    SpeechEnded,
    PlaybackFinished(String),
    Stop,
    Failure(VoiceError),
    ResponseFinished(u64),
    Settle,
}

pub async fn run_voice_session(
    providers: ProviderSet,
    mut context: CallContext,
    mut input: mpsc::Receiver<CallEvent>,
    output: mpsc::Sender<CallCommand>,
) -> Result<(), VoiceError> {
    let stt = providers.stt.connect().await?;
    let (signal_tx, mut signal_rx) = mpsc::channel(32);
    let input_stt = stt.clone();
    let input_signal = signal_tx.clone();

    let last_audio_at = Arc::new(std::sync::Mutex::new(None));
    let input_last_audio = last_audio_at.clone();

    let speech_accumulator = Arc::new(std::sync::Mutex::new(
        crate::voice::embedding::SpeechAccumulator::new(),
    ));
    let input_speech_accumulator = speech_accumulator.clone();

    let mut vad = crate::voice::vad::VoiceActivityDetector::new();
    let input_task = tokio::spawn(async move {
        while let Some(event) = input.recv().await {
            match event {
                CallEvent::Audio(audio) => {
                    input_speech_accumulator.lock().unwrap().push_frame(&audio);
                    let vad_event = vad.process_frame(&audio);
                    if vad.current_rms() >= vad.dynamic_threshold() {
                        *input_last_audio.lock().unwrap() = Some(std::time::Instant::now());
                    }
                    if vad_event == crate::voice::vad::VadEvent::SpeechStarted {
                        tracing::info!(
                            rms = %vad.current_rms(),
                            threshold = %vad.dynamic_threshold(),
                            noise_floor = %vad.noise_floor(),
                            "VAD: Inbound speech onset detected (SpeechStarted)"
                        );
                        let _ = input_signal.send(SessionSignal::SpeechDetected).await;
                    }
                    if vad_event == crate::voice::vad::VadEvent::SpeechEnded {
                        let _ = input_signal.send(SessionSignal::SpeechEnded).await;
                    }
                    if let Err(error) = input_stt.send_audio(audio).await {
                        let _ = input_signal.send(SessionSignal::Failure(error)).await;
                        return;
                    }
                }
                CallEvent::PlaybackFinished(name) => {
                    if input_signal
                        .send(SessionSignal::PlaybackFinished(name))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                CallEvent::Stop => {
                    let _ = input_signal.send(SessionSignal::Stop).await;
                    return;
                }
            }
        }
        let _ = input_signal.send(SessionSignal::Stop).await;
    });
    let event_stt = stt.clone();
    let event_signal = signal_tx.clone();
    let stt_task = tokio::spawn(async move {
        loop {
            match event_stt.next_event().await {
                Ok(Some(event)) => {
                    if event_signal.send(SessionSignal::Stt(event)).await.is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = event_signal.send(SessionSignal::Stop).await;
                    return;
                }
                Err(error) => {
                    let _ = event_signal.send(SessionSignal::Failure(error)).await;
                    return;
                }
            }
        }
    });
    let (speculation_tx, mut speculation_rx) = tokio::sync::watch::channel::<Option<(CallContext, String)>>(None);
    let speculation_agent = providers.agent.clone();
    let speculation_task = tokio::spawn(async move {
        while speculation_rx.changed().await.is_ok() {
            loop {
                tokio::select! {
                    result = speculation_rx.changed() => { if result.is_err() { return; } }
                    _ = tokio::time::sleep(Duration::from_millis(250)) => break,
                }
            }
            let snapshot = speculation_rx.borrow_and_update().clone();
            if let Some((ctx, text)) = snapshot {
                tokio::select! {
                    result = speculation_agent.speculate(&ctx, &text) => {
                        if let Err(err) = result { tracing::debug!(error = %err, "VOICE_SPECULATION_UNAVAILABLE"); }
                    }
                    result = speculation_rx.changed() => {
                        if result.is_err() { return; }
                        speculation_rx.mark_changed();
                    }
                }
            }
        }
    });
    let mut active_response: Option<JoinHandle<()>> = None;
    let mut response_number = 0_u64;
    let mut draft = crate::voice::turn::DraftTurn::default();
    let mut draft_timing: Option<TurnTiming> = None;
    let mut dispatched: Option<(crate::voice::turn::DraftTurn, TurnTiming)> = None;
    let mut settle_at: Option<tokio::time::Instant> = None;
    let mut speech_started_at: Option<std::time::Instant> = None;
    let mut caller_speaking = false;
    let mut failure = None;
    let audio_playing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let answer_started = Arc::new(std::sync::atomic::AtomicBool::new(false));

    if let Some(ref opening) = context.initiation_context {
        response_number += 1;
        active_response = Some(spawn_response(
            response_number, context.clone(), opening.clone(), None, None,
            providers.agent.clone(), providers.tts.clone(),
            output.clone(), signal_tx.clone(), audio_playing.clone(), answer_started.clone(),
        ));
    }

    loop {
        let signal = tokio::select! {
            biased;
            signal = signal_rx.recv() => match signal { Some(signal) => signal, None => break },
            _ = async {
                match settle_at {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            }, if !caller_speaking => SessionSignal::Settle,
        };
        match signal {
            onset @ (SessionSignal::SpeechDetected | SessionSignal::Stt(SttEvent::SpeechStarted)) => {
                if caller_speaking { continue; }
                caller_speaking = true;
                settle_at = None;
                speech_started_at.get_or_insert_with(std::time::Instant::now);
                let is_playing = audio_playing.swap(false, std::sync::atomic::Ordering::SeqCst);
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
                if is_playing {
                    if answer_started.load(std::sync::atomic::Ordering::SeqCst) { dispatched = None; }
                    output.send(CallCommand::Clear).await.map_err(|_| VoiceError::Protocol("call output closed".into()))?;
                }
                tracing::info!(turn = response_number, playing = is_playing, source = if matches!(onset, SessionSignal::SpeechDetected) { "vad" } else { "stt" }, "VOICE_TURN_INTERRUPTED");
            }
            SessionSignal::SpeechEnded => {
                caller_speaking = false;
                if !draft.text.is_empty() && !draft.has_partial() { settle_at = Some(tokio::time::Instant::now() + draft.settle_delay()); }
            }
            SessionSignal::PlaybackFinished(name) => {
                if name == format!("filler-{response_number}") && !answer_started.load(std::sync::atomic::Ordering::SeqCst) {
                    audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
                }
                if name == format!("response-{response_number}") {
                    audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
                    dispatched = None;
                    tracing::info!(turn = response_number, conversation_id = %context.external_conversation_id, "VOICE_PLAYBACK_ACKNOWLEDGED");
                }
            }
            SessionSignal::Stt(SttEvent::PartialTranscript(text)) => {
                if draft.partial(&text) && draft.snapshot().split_whitespace().count() >= 3 {
                    let mut ctx = context.clone();
                    ctx.turn_id = Some(draft.id.clone());
                    ctx.revision = Some(draft.revision);
                    let _ = speculation_tx.send(Some((ctx, draft.snapshot())));
                }
                settle_at = None;
            }
            SessionSignal::Stt(SttEvent::FinalTranscript(text)) => {
                if text.trim().is_empty() { continue; }
                caller_speaking = false;
                let now = std::time::Instant::now();
                let last_audio = *last_audio_at.lock().unwrap();
                let timing = draft_timing.get_or_insert(TurnTiming {
                    speech_started_at: speech_started_at.take(), last_audio_at: last_audio, transcript_received_at: now,
                });
                timing.last_audio_at = last_audio;
                if let Some(task) = active_response.take() {
                    task.abort();
                    let _ = task.await;
                    if !answer_started.load(std::sync::atomic::Ordering::SeqCst)
                        && draft.text.is_empty()
                        && let Some((previous, previous_timing)) = dispatched.take()
                    {
                        draft = previous;
                        draft_timing = Some(TurnTiming { last_audio_at: last_audio, ..previous_timing });
                    }
                }
                if audio_playing.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    output.send(CallCommand::Clear).await.map_err(|_| VoiceError::Protocol("call output closed".into()))?;
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
            SessionSignal::Settle => {
                settle_at = None;
                if draft.text.is_empty() { continue; }
                let turn = std::mem::take(&mut draft);
                let timing = draft_timing.take().unwrap_or(TurnTiming {
                    speech_started_at: None, last_audio_at: None, transcript_received_at: std::time::Instant::now(),
                });
                context.turn_id = Some(turn.id.clone());
                context.revision = Some(turn.revision);
                context.voice_signature = None;
                let audio = speech_accumulator.lock().unwrap().take_audio();
                response_number += 1;
                answer_started.store(false, std::sync::atomic::Ordering::SeqCst);
                dispatched = Some((turn.clone(), timing.clone()));
                active_response = Some(spawn_response(
                    response_number, context.clone(), turn.text, Some(timing), Some(audio),
                    providers.agent.clone(), providers.tts.clone(),
                    output.clone(), signal_tx.clone(), audio_playing.clone(), answer_started.clone(),
                ));
            }
            SessionSignal::ResponseFinished(number) => {
                if number == response_number {
                    active_response.take();
                    if !audio_playing.load(std::sync::atomic::Ordering::SeqCst) { dispatched = None; }
                }
            }
            SessionSignal::Stop => break,
            SessionSignal::Failure(error) => { failure = Some(error); break; }
        }
    }
    stop_task(speculation_task).await;
    if let Some(task) = active_response {
        task.abort();
    }
    stop_task(input_task).await;
    stop_task(stt_task).await;
    let finish_result = tokio::time::timeout(Duration::from_secs(3), stt.finish())
        .await
        .map_err(|_| VoiceError::Timeout("AssemblyAI termination"))?;
    if let Some(error) = failure {
        return Err(error);
    }
    finish_result
}

struct ResponseLifecycle {
    number: u64,
    conversation_id: String,
    started: std::time::Instant,
    outcome: &'static str,
}

impl Drop for ResponseLifecycle {
    fn drop(&mut self) {
        tracing::info!(turn = self.number, conversation_id = %self.conversation_id, outcome = self.outcome, elapsed_ms = self.started.elapsed().as_millis(), "VOICE_RESPONSE_ENDED");
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_response(
    number: u64,
    mut context: CallContext,
    transcript: String,
    timing: Option<TurnTiming>,
    audio: Option<Vec<u8>>,
    agent: Arc<dyn crate::voice::provider::AgentProvider>,
    tts: Arc<dyn crate::voice::provider::TtsProvider>,
    output: mpsc::Sender<CallCommand>,
    signal: mpsc::Sender<SessionSignal>,
    audio_playing: Arc<std::sync::atomic::AtomicBool>,
    answer_started: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lifecycle = ResponseLifecycle {
            number,
            conversation_id: context.external_conversation_id.clone(),
            started: std::time::Instant::now(),
            outcome: "cancelled",
        };
        if let Some(audio) = audio {
            let started = std::time::Instant::now();
            context.voice_signature = tokio::task::spawn_blocking(move || crate::voice::embedding::extract_audio_signature(audio)).await.ok().flatten();
            tracing::info!(turn = number, elapsed_ms = started.elapsed().as_millis(), available = context.voice_signature.is_some(), "VOICE_EMBEDDING_FINISHED");
        }
        let result = stream_response(
            number,
            &context,
            &transcript,
            timing,
            agent,
            tts,
            output,
            audio_playing.clone(),
            answer_started,
        )
        .await;
        lifecycle.outcome = if result.is_ok() {
            "completed"
        } else {
            "failed"
        };
        if let Err(error) = result {
            audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
            tracing::warn!(provider_error = %error, "voice response failed");
        }
        let _ = signal.send(SessionSignal::ResponseFinished(number)).await;
    })
}

use crate::voice::filler::{play_filler, strip_leading_ack};

#[allow(clippy::too_many_arguments)]
async fn stream_response(
    number: u64,
    context: &CallContext,
    transcript: &str,
    timing: Option<TurnTiming>,
    agent: Arc<dyn crate::voice::provider::AgentProvider>,
    tts: Arc<dyn crate::voice::provider::TtsProvider>,
    output: mpsc::Sender<CallCommand>,
    audio_playing: Arc<std::sync::atomic::AtomicBool>,
    answer_started: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), VoiceError> {
    let turn_started_at = std::time::Instant::now();
    let llm_request_start = std::time::Instant::now();

    tracing::info!(
        turn = number,
        conversation_id = %context.external_conversation_id,
        prompt = %transcript,
        "Voice pipeline: processing turn"
    );

    let mut chunker = crate::voice::chunker::SentenceChunker::new();
    let mut first_token_at: Option<std::time::Instant> = None;
    let mut first_sentence_at: Option<std::time::Instant> = None;
    let mut first_sentence_text: Option<String> = None;
    let mut first_audio_sent_at: Option<std::time::Instant> = None;
    let mut first_tts_ttfb_ms: Option<u128> = None;
    let mut full_response = String::new();
    let mut sentence_count = 0usize;

    let mut filler_played = false;
    let mut filler_audio_at = None;
    let mut lookup_deadline = None;
    let mut text_stream = tokio::time::timeout(Duration::from_secs(30), agent.respond_events(context, transcript))
        .await.map_err(|_| VoiceError::Timeout("Core response"))??;

    loop {
        let next = tokio::select! {
            biased;
            result = tokio::time::timeout(Duration::from_secs(30), text_stream.next()) => {
                result.map_err(|_| VoiceError::Timeout("Core stream"))?
            }
            _ = async {
                match lookup_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                lookup_deadline = None;
                filler_played = true;
                play_filler("I'm looking into that.", tts.as_ref(), &output, &mut filler_audio_at, &audio_playing).await?;
                output.send(CallCommand::Mark(format!("filler-{number}"))).await.map_err(|_| VoiceError::Protocol("call output closed".into()))?;
                tracing::info!(turn = number, filler_audio_ms = ?filler_audio_at.map(|at| at.duration_since(turn_started_at).as_millis()), "VOICE_LOOKUP_ACKNOWLEDGED");
                continue;
            }
        };
        let Some(event) = next else { break; };
        let chunk = match event? {
            crate::voice::provider::AgentEvent::LookupPending => {
                if !filler_played && first_token_at.is_none() && lookup_deadline.is_none() {
                    lookup_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(400));
                }
                continue;
            }
            crate::voice::provider::AgentEvent::Text(text) => {
                if text.trim().is_empty() { continue; }
                lookup_deadline = None;
                text
            }
        };
        if first_token_at.is_none() && !chunk.trim().is_empty() {
            first_token_at = Some(std::time::Instant::now());
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
            if sentence_count == 1 || first_sentence_at.is_none() {
                first_sentence_at = Some(std::time::Instant::now());
                first_sentence_text = Some(trimmed.to_string());
            }
            match play_sentence(
                trimmed,
                tts.as_ref(),
                &output,
                &mut first_audio_sent_at,
                &audio_playing,
                &answer_started,
            )
            .await
            {
                Ok(ttfb) => {
                    if first_tts_ttfb_ms.is_none() {
                        first_tts_ttfb_ms = Some(ttfb);
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, sentence = %trimmed, "TTS synthesis failed for sentence, continuing turn");
                }
            }
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
                if sentence_count == 1 || first_sentence_at.is_none() {
                    first_sentence_at = Some(std::time::Instant::now());
                    first_sentence_text = Some(trimmed.to_string());
                }
                match play_sentence(
                    trimmed,
                    tts.as_ref(),
                    &output,
                    &mut first_audio_sent_at,
                    &audio_playing,
                    &answer_started,
                )
                .await
                {
                    Ok(ttfb) => {
                        if first_tts_ttfb_ms.is_none() {
                            first_tts_ttfb_ms = Some(ttfb);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, sentence = %trimmed, "TTS synthesis failed for flushed sentence, continuing turn");
                    }
                }
            }
        }
    }

    if first_audio_sent_at.is_some() || filler_audio_at.is_some() {
        output
            .send(CallCommand::Mark(format!("response-{number}")))
            .await
            .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
    }

    let all_audio_sent_at = std::time::Instant::now();

    log_turn_latency(
        number,
        context,
        transcript,
        &full_response,
        first_sentence_text.as_deref(),
        timing,
        turn_started_at,
        llm_request_start,
        first_token_at,
        first_sentence_at,
        first_tts_ttfb_ms,
        first_audio_sent_at,
        all_audio_sent_at,
    );

    Ok(())
}

async fn play_sentence(
    sentence: &str,
    tts: &dyn crate::voice::provider::TtsProvider,
    output: &mpsc::Sender<CallCommand>,
    first_audio_tracker: &mut Option<std::time::Instant>,
    audio_playing: &std::sync::atomic::AtomicBool,
    answer_started: &std::sync::atomic::AtomicBool,
) -> Result<u128, VoiceError> {
    let text = sentence.trim();
    if text.is_empty() || !text.chars().any(|c| c.is_alphabetic()) {
        return Ok(0);
    }
    let tts_start = std::time::Instant::now();
    let mut audio = tts.synthesize(text).await?;
    let mut ttfb_ms = 0;
    let mut is_first_chunk = true;

    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(10), audio.next())
        .await
        .map_err(|_| VoiceError::Timeout("TTS audio"))?
    {
        let chunk_bytes = chunk?;
        output
            .send(CallCommand::Media(chunk_bytes))
            .await
            .map_err(|_| VoiceError::Protocol("call output closed".into()))?;

        answer_started.store(true, std::sync::atomic::Ordering::SeqCst);
        audio_playing.store(true, std::sync::atomic::Ordering::SeqCst);

        let now = std::time::Instant::now();
        if is_first_chunk {
            ttfb_ms = now.duration_since(tts_start).as_millis();
            if first_audio_tracker.is_none() {
                *first_audio_tracker = Some(now);
            }
            is_first_chunk = false;
        }
    }
    Ok(ttfb_ms)
}

#[allow(clippy::too_many_arguments)]
fn log_turn_latency(
    number: u64,
    context: &CallContext,
    prompt: &str,
    full_response: &str,
    first_sentence: Option<&str>,
    timing: Option<TurnTiming>,
    turn_started_at: std::time::Instant,
    llm_request_start: std::time::Instant,
    first_token_at: Option<std::time::Instant>,
    first_sentence_at: Option<std::time::Instant>,
    first_tts_ttfb_ms: Option<u128>,
    first_audio_sent_at: Option<std::time::Instant>,
    all_audio_sent_at: std::time::Instant,
) {
    let transcript_rx_at = timing
        .as_ref()
        .map(|t| t.transcript_received_at)
        .unwrap_or(turn_started_at);

    let stt_speech_duration_ms = timing.as_ref().and_then(|t| {
        t.speech_started_at
            .map(|s| t.last_audio_at.unwrap_or(t.transcript_received_at).saturating_duration_since(s).as_millis())
    });

    let stt_endpointing_ms = timing.as_ref().and_then(|t| {
        t.last_audio_at
            .map(|a| t.transcript_received_at.saturating_duration_since(a).as_millis())
    });

    let queue_wait_ms = turn_started_at.duration_since(transcript_rx_at).as_millis();

    let llm_ttft_ms = first_token_at.map(|ft| ft.duration_since(llm_request_start).as_millis());
    let llm_ttfs_ms = first_sentence_at.map(|fs| fs.duration_since(llm_request_start).as_millis());
    let tts_ttfb_ms = first_tts_ttfb_ms;

    // Time from transcript received to first audio dispatched to Twilio (pipeline latency)
    let time_to_first_audio_ms =
        first_audio_sent_at.map(|fa| fa.duration_since(transcript_rx_at).as_millis());

    // Time from caller stopped speaking to first audio dispatched to Twilio (user perceived delay)
    let user_perceived_delay_ms = timing.as_ref().and_then(|t| {
        t.last_audio_at
            .and_then(|la| first_audio_sent_at.map(|fa| fa.duration_since(la).as_millis()))
    });

    let total_turn_duration_ms = all_audio_sent_at
        .duration_since(transcript_rx_at)
        .as_millis();

    // 1. Structured trace log for monitoring and metrics aggregation
    tracing::info!(
        turn = number,
        conversation_id = %context.external_conversation_id,
        channel = %context.channel,
        prompt = %prompt,
        first_sentence = first_sentence.unwrap_or(""),
        time_to_first_answer_audio_ms = ?time_to_first_audio_ms,
        user_perceived_delay_ms = ?user_perceived_delay_ms,
        stt_speech_duration_ms = ?stt_speech_duration_ms,
        stt_endpointing_ms = ?stt_endpointing_ms,
        queue_wait_ms,
        core_first_text_ms = ?llm_ttft_ms,
        core_first_sentence_ms = ?llm_ttfs_ms,
        tts_ttfb_ms = ?tts_ttfb_ms,
        total_turn_duration_ms,
        "VOICE_PIPELINE_METRICS"
    );

    // 2. High-visibility summary card for prompt benchmark reports
    let stt_speech_str = stt_speech_duration_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "N/A".into());
    let stt_endpoint_str = stt_endpointing_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "N/A".into());
    let llm_ttft_str = llm_ttft_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "N/A".into());
    let llm_ttfs_str = llm_ttfs_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "N/A".into());
    let tts_ttfb_str = tts_ttfb_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "N/A".into());
    let ttfa_str = time_to_first_audio_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "N/A".into());
    let user_delay_str = user_perceived_delay_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "N/A".into());

    tracing::info!(
        "\n======================= [VOICE PIPELINE LATENCY REPORT] =======================\n\
         Turn #{number} | Call: {conv_id}\n\
         Prompt (User Question):    \"{prompt}\"\n\
         First Sentence (Response): \"{first_sentence_display}\"\n\
         Full Response Text:        \"{full_resp_display}\"\n\
         -------------------------------------------------------------------------------\n\
         Pipeline Latency Breakdown:\n\
           • STT Speech Active:            {stt_speech_str}\n\
           • STT Endpointing (Silence):    {stt_endpoint_str}\n\
           • Queue Delay:                  {queue_wait_ms} ms\n\
           • Core Time to 1st Text: {llm_ttft_str}\n\
           • Core Time to 1st Sentence:     {llm_ttfs_str}\n\
           • TTS Time to 1st Audio (TTFB): {tts_ttfb_str}\n\
         -------------------------------------------------------------------------------\n\
           ★ TIME TO FIRST AUDIO (pipeline processing):   {ttfa_str}\n\
           ★ TOTAL USER-PERCEIVED DELAY (from speech end): {user_delay_str}\n\
           • Total Turn Duration (full response audio):    {total_turn_duration_ms} ms\n\
         ===============================================================================",
        conv_id = context.external_conversation_id,
        first_sentence_display = first_sentence.unwrap_or("").trim(),
        full_resp_display = full_response.trim(),
    );
}

async fn stop_task(task: JoinHandle<()>) {
    task.abort();
    let _ = task.await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::{
        provider::{AgentProvider, AudioStream, SttProvider, SttSession, TtsProvider},
        registry::ProviderSet,
    };
    use async_trait::async_trait;
    use futures_util::stream;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::Notify;
    use tokio::sync::{Mutex, mpsc};

    struct FakeSttProvider {
        session: Arc<FakeSttSession>,
    }

    struct FakeSttSession {
        audio: Mutex<Vec<Bytes>>,
        events: Mutex<mpsc::Receiver<SttEvent>>,
        finished: AtomicBool,
    }

    struct FakeAgent {
        transcripts: Mutex<Vec<String>>,
        response: Mutex<String>,
        first_gate: Mutex<Option<Arc<Notify>>>,
    }

    struct FakeTts {
        texts: Mutex<Vec<String>>,
        chunks: Vec<Bytes>,
        pending: AtomicBool,
        fail: AtomicBool,
    }

    #[async_trait]
    impl SttProvider for FakeSttProvider {
        async fn connect(&self) -> Result<Arc<dyn SttSession>, VoiceError> {
            Ok(self.session.clone())
        }
    }

    #[async_trait]
    impl SttSession for FakeSttSession {
        async fn send_audio(&self, audio: Bytes) -> Result<(), VoiceError> {
            self.audio.lock().await.push(audio);
            Ok(())
        }

        async fn next_event(&self) -> Result<Option<SttEvent>, VoiceError> {
            Ok(self.events.lock().await.recv().await)
        }

        async fn finish(&self) -> Result<(), VoiceError> {
            self.finished.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl AgentProvider for FakeAgent {
        async fn respond(
            &self,
            _context: &CallContext,
            transcript: &str,
        ) -> Result<String, VoiceError> {
            self.transcripts.lock().await.push(transcript.into());
            if transcript == "first"
                && let Some(gate) = self.first_gate.lock().await.clone()
            {
                gate.notified().await;
            }
            Ok(self.response.lock().await.clone())
        }
    }

    fn call_context() -> CallContext {
        CallContext {
            channel: "phone".into(),
            external_identity: "+14155550100".into(),
            external_conversation_id: "CA123".into(),
            initiation_context: None,
            voice_signature: None,
            turn_id: None,
            revision: None,
            tts_provider: None,
        }
    }

    #[async_trait]
    impl TtsProvider for FakeTts {
        async fn synthesize(&self, text: &str) -> Result<AudioStream, VoiceError> {
            self.texts.lock().await.push(text.into());
            if self.fail.load(Ordering::SeqCst) {
                return Err(VoiceError::Provider {
                    provider: "fake-tts",
                    message: "failed".into(),
                });
            }
            let chunks = stream::iter(self.chunks.clone().into_iter().map(Ok));
            if self.pending.load(Ordering::SeqCst) {
                Ok(Box::pin(chunks.chain(stream::pending())))
            } else {
                Ok(Box::pin(chunks))
            }
        }
    }

    fn providers() -> (
        ProviderSet,
        mpsc::Sender<SttEvent>,
        Arc<FakeSttSession>,
        Arc<FakeAgent>,
        Arc<FakeTts>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(8);
        let stt_session = Arc::new(FakeSttSession {
            audio: Mutex::new(Vec::new()),
            events: Mutex::new(event_rx),
            finished: AtomicBool::new(false),
        });
        let agent = Arc::new(FakeAgent {
            transcripts: Mutex::new(Vec::new()),
            response: Mutex::new("Hi there".into()),
            first_gate: Mutex::new(None),
        });
        let tts = Arc::new(FakeTts {
            texts: Mutex::new(Vec::new()),
            chunks: vec![Bytes::from_static(&[1, 2]), Bytes::from_static(&[3, 4])],
            pending: AtomicBool::new(false),
            fail: AtomicBool::new(false),
        });
        (
            ProviderSet {
                stt: Arc::new(FakeSttProvider {
                    session: stt_session.clone(),
                }),
                agent: agent.clone(),
                tts: tts.clone(),
            },
            event_tx,
            stt_session,
            agent,
            tts,
        )
    }

    #[tokio::test]
    async fn filler_remains_interruptible_until_playback_ack() {
        let (_, _, _, _, tts) = providers();
        let (output, _receiver) = mpsc::channel(8);
        for _ in 0..2 {
            let playing = AtomicBool::new(false);
            let mut first_audio = None;
            play_filler(
                "Test filler playback.",
                tts.as_ref(),
                &output,
                &mut first_audio,
                &playing,
            )
            .await
            .unwrap();
            assert!(first_audio.is_some());
            assert!(
                playing.load(Ordering::SeqCst),
                "buffered filler must remain interruptible until acknowledged"
            );
        }
    }

    #[tokio::test]
    async fn interruption_clears_audio_even_after_generation_finishes() {
        let (providers, event_tx, _, _, _) = providers();
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));
        event_tx
            .send(SttEvent::FinalTranscript("first".into()))
            .await
            .unwrap();
        for _ in 0..3 {
            output_rx.recv().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        event_tx.send(SttEvent::SpeechStarted).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), output_rx.recv())
                .await
                .unwrap(),
            Some(CallCommand::Clear)
        );
        event_tx.send(SttEvent::SpeechStarted).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), output_rx.recv())
                .await
                .is_err()
        );
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn paused_fragments_are_one_turn_and_stale_marks_do_not_end_playback() {
        let (providers, event_tx, _, agent, _) = providers();
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(16);
        let session = tokio::spawn(run_voice_session(providers, call_context(), input_rx, output_tx));
        event_tx.send(SttEvent::FinalTranscript("list my tasks".into())).await.unwrap();
        event_tx.send(SttEvent::SpeechStarted).await.unwrap();
        event_tx.send(SttEvent::FinalTranscript("for tomorrow".into())).await.unwrap();
        for _ in 0..3 { tokio::time::timeout(Duration::from_secs(2), output_rx.recv()).await.unwrap().unwrap(); }
        assert_eq!(*agent.transcripts.lock().await, vec!["list my tasks for tomorrow"]);
        input_tx.send(CallEvent::PlaybackFinished("response-0".into())).await.unwrap();
        event_tx.send(SttEvent::SpeechStarted).await.unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(1), output_rx.recv()).await.unwrap(), Some(CallCommand::Clear));
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn local_silence_releases_queue_when_noise_has_no_transcript() {
        let (providers, event_tx, _, agent, _) = providers();
        let (input_tx, input_rx) = mpsc::channel(32);
        let (output_tx, mut output_rx) = mpsc::channel(16);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));
        event_tx
            .send(SttEvent::FinalTranscript("first".into()))
            .await
            .unwrap();
        for _ in 0..3 {
            output_rx.recv().await.unwrap();
        }
        event_tx
            .send(SttEvent::FinalTranscript("second".into()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        for _ in 0..2 {
            input_tx
                .send(CallEvent::Audio(Bytes::from(vec![0x90; 160])))
                .await
                .unwrap();
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
                .await
                .unwrap(),
            Some(CallCommand::Clear)
        );
        for _ in 0..16 {
            input_tx
                .send(CallEvent::Audio(Bytes::from(vec![0xff; 160])))
                .await
                .unwrap();
        }
        for _ in 0..3 {
            tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(
            agent.transcripts.lock().await.as_slice(),
            &["first", "second"]
        );
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn turns_call_audio_into_marked_response_audio() {
        let (providers, event_tx, stt, agent, tts) = providers();
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));

        input_tx
            .send(CallEvent::Audio(Bytes::from_static(&[0xff, 0x7f])))
            .await
            .unwrap();
        event_tx
            .send(SttEvent::FinalTranscript("hello".into()))
            .await
            .unwrap();

        let mut commands = Vec::new();
        for _ in 0..3 {
            commands.push(output_rx.recv().await.unwrap());
        }
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();

        assert_eq!(
            stt.audio.lock().await.as_slice(),
            &[Bytes::from_static(&[0xff, 0x7f])]
        );
        assert_eq!(agent.transcripts.lock().await.as_slice(), &["hello"]);
        assert_eq!(tts.texts.lock().await.as_slice(), &["Hi there"]);
        assert_eq!(
            commands,
            vec![
                CallCommand::Media(Bytes::from_static(&[1, 2])),
                CallCommand::Media(Bytes::from_static(&[3, 4])),
                CallCommand::Mark("response-1".into()),
            ]
        );
        assert!(stt.finished.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn speech_started_cancels_playback_and_clears_twilio() {
        let (providers, event_tx, _, agent, tts) = providers();
        tts.pending.store(true, Ordering::SeqCst);
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));
        event_tx
            .send(SttEvent::FinalTranscript("first".into()))
            .await
            .unwrap();
        assert_eq!(
            output_rx.recv().await,
            Some(CallCommand::Media(Bytes::from_static(&[1, 2])))
        );
        assert_eq!(
            output_rx.recv().await,
            Some(CallCommand::Media(Bytes::from_static(&[3, 4])))
        );

        event_tx.send(SttEvent::SpeechStarted).await.unwrap();
        let clear = tokio::time::timeout(std::time::Duration::from_millis(100), output_rx.recv())
            .await
            .unwrap();
        assert_eq!(clear, Some(CallCommand::Clear));

        tts.pending.store(false, Ordering::SeqCst);
        event_tx
            .send(SttEvent::FinalTranscript("second".into()))
            .await
            .unwrap();
        for _ in 0..3 {
            output_rx.recv().await.unwrap();
        }
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
        assert_eq!(
            agent.transcripts.lock().await.as_slice(),
            &["first", "second"]
        );
    }

    #[tokio::test]
    async fn local_vad_speech_detected_cancels_playback_and_clears_twilio() {
        let (providers, event_tx, _, _, tts) = providers();
        tts.pending.store(true, Ordering::SeqCst);
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));
        event_tx
            .send(SttEvent::FinalTranscript("first".into()))
            .await
            .unwrap();
        assert_eq!(
            output_rx.recv().await,
            Some(CallCommand::Media(Bytes::from_static(&[1, 2])))
        );
        assert_eq!(
            output_rx.recv().await,
            Some(CallCommand::Media(Bytes::from_static(&[3, 4])))
        );

        // Send two frames of high-energy speech audio (0x90 expands to ~2000 linear RMS)
        input_tx
            .send(CallEvent::Audio(Bytes::from(vec![0x90; 160])))
            .await
            .unwrap();
        input_tx
            .send(CallEvent::Audio(Bytes::from(vec![0x90; 160])))
            .await
            .unwrap();

        let clear = tokio::time::timeout(std::time::Duration::from_millis(200), output_rx.recv())
            .await
            .unwrap();
        assert_eq!(clear, Some(CallCommand::Clear));

        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn correction_cancels_pending_response_instead_of_queuing_behind_it() {
        let (providers, event_tx, _, agent, _) = providers();
        *agent.first_gate.lock().await = Some(Arc::new(Notify::new()));
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(16);
        let session = tokio::spawn(run_voice_session(providers, call_context(), input_rx, output_tx));
        event_tx.send(SttEvent::FinalTranscript("first".into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while agent.transcripts.lock().await.is_empty() { tokio::task::yield_now().await; }
        }).await.unwrap();
        event_tx.send(SttEvent::SpeechStarted).await.unwrap();
        event_tx.send(SttEvent::FinalTranscript("Actually, second".into())).await.unwrap();
        for _ in 0..3 { tokio::time::timeout(Duration::from_secs(2), output_rx.recv()).await.unwrap().unwrap(); }
        assert_eq!(*agent.transcripts.lock().await, vec!["first", "Actually, second"]);
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_failed_response_does_not_leak_or_end_the_call() {
        let (providers, event_tx, stt, _, tts) = providers();
        tts.fail.store(true, Ordering::SeqCst);
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));
        event_tx
            .send(SttEvent::FinalTranscript("first".into()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(output_rx.try_recv().is_err());

        tts.fail.store(false, Ordering::SeqCst);
        event_tx
            .send(SttEvent::FinalTranscript("second".into()))
            .await
            .unwrap();
        for _ in 0..3 {
            output_rx.recv().await.unwrap();
        }
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
        assert!(stt.finished.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn multi_sentence_response_is_synthesized_and_streamed_per_sentence() {
        let (providers, event_tx, _, agent, tts) = providers();
        *agent.response.lock().await = "First sentence. Second sentence.".into();
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(16);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));

        event_tx
            .send(SttEvent::FinalTranscript("hello".into()))
            .await
            .unwrap();

        let mut commands = Vec::new();
        // 2 chunks for first sentence, 2 chunks for second sentence, 1 mark
        for _ in 0..5 {
            commands.push(output_rx.recv().await.unwrap());
        }
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();

        assert_eq!(
            tts.texts.lock().await.as_slice(),
            &["First sentence.", "Second sentence."]
        );
        assert_eq!(
            commands,
            vec![
                CallCommand::Media(Bytes::from_static(&[1, 2])),
                CallCommand::Media(Bytes::from_static(&[3, 4])),
                CallCommand::Media(Bytes::from_static(&[1, 2])),
                CallCommand::Media(Bytes::from_static(&[3, 4])),
                CallCommand::Mark("response-1".into()),
            ]
        );
    }

    #[test]
    fn test_strip_leading_ack() {
        assert_eq!(
            strip_leading_ack("On it. India's next match is a Test against the West Indies."),
            "India's next match is a Test against the West Indies."
        );
        assert_eq!(
            strip_leading_ack("Done. Apple is trading at $337."),
            "Apple is trading at $337."
        );
        assert_eq!(
            strip_leading_ack("Sure thing! I will set that reminder."),
            "I will set that reminder."
        );
        assert_eq!(
            strip_leading_ack("Araku Coffee in Indiranagar is a fantastic spot."),
            "Araku Coffee in Indiranagar is a fantastic spot."
        );
    }
}
