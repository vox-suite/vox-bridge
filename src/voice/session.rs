use crate::voice::{
    context::CallContext,
    provider::{SttEvent, VoiceError},
    registry::ProviderSet,
};
use bytes::Bytes;
use futures_util::StreamExt;
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEvent {
    Audio(Bytes),
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
    Stop,
    Failure(VoiceError),
    ResponseFinished(u64),
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
                    *input_last_audio.lock().unwrap() = Some(std::time::Instant::now());
                    input_speech_accumulator.lock().unwrap().push_frame(&audio);
                    let vad_event = vad.process_frame(&audio);
                    if vad_event == crate::voice::vad::VadEvent::SpeechStarted {
                        tracing::info!(
                            rms = %vad.current_rms(),
                            threshold = %vad.dynamic_threshold(),
                            noise_floor = %vad.noise_floor(),
                            "VAD: Inbound speech onset detected (SpeechStarted)"
                        );
                        let _ = input_signal.send(SessionSignal::SpeechDetected).await;
                    }
                    if let Err(error) = input_stt.send_audio(audio).await {
                        let _ = input_signal.send(SessionSignal::Failure(error)).await;
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
    let mut active_response: Option<JoinHandle<()>> = None;
    let mut response_number = 0_u64;
    let mut pending_transcripts: VecDeque<(String, TurnTiming)> = VecDeque::new();
    let mut speech_started_at: Option<std::time::Instant> = None;
    let mut failure = None;
    let audio_playing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut current_active_prompt: Option<String> = None;
    let mut interrupted_prompt_buffer: Option<String> = None;

    if let Some(ref opening) = context.initiation_context {
        response_number += 1;
        current_active_prompt = Some(opening.clone());
        active_response = Some(spawn_response(
            response_number,
            context.clone(),
            opening.clone(),
            None,
            providers.agent.clone(),
            providers.tts.clone(),
            providers.jev.clone(),
            output.clone(),
            signal_tx.clone(),
            audio_playing.clone(),
        ));
    }

    while let Some(signal) = signal_rx.recv().await {
        match signal {
            SessionSignal::SpeechDetected => {
                if speech_started_at.is_none() {
                    speech_started_at = Some(std::time::Instant::now());
                }
                let is_playing = audio_playing.load(std::sync::atomic::Ordering::SeqCst);
                let has_active_response = active_response.is_some();
                tracing::info!(
                    is_playing,
                    has_active_response,
                    "Voice session: VAD SpeechDetected event received"
                );
                // Only execute barge-in interruption if the assistant is actively playing audio aloud
                if has_active_response && is_playing {
                    tracing::info!(
                        "Local VAD: Voice detected during active playback, executing barge-in interruption"
                    );
                    if let Some(task) = active_response.take() {
                        task.abort();
                        output
                            .send(CallCommand::Clear)
                            .await
                            .map_err(|_| VoiceError::Protocol("call output closed".into()))?;

                        let was_speaking =
                            audio_playing.swap(false, std::sync::atomic::Ordering::SeqCst);
                        if let Some(interrupted) = current_active_prompt.take() {
                            if !was_speaking && !interrupted.starts_with("The call just connected")
                            {
                                interrupted_prompt_buffer =
                                    Some(match interrupted_prompt_buffer.take() {
                                        Some(prev) => format!("{prev} {interrupted}"),
                                        None => interrupted,
                                    });
                            } else {
                                interrupted_prompt_buffer = None;
                            }
                        }
                    }
                    if let Some((transcript, timing)) = pending_transcripts.pop_front() {
                        response_number += 1;
                        current_active_prompt = Some(transcript.clone());
                        active_response = Some(spawn_response(
                            response_number,
                            context.clone(),
                            transcript,
                            Some(timing),
                            providers.agent.clone(),
                            providers.tts.clone(),
                            providers.jev.clone(),
                            output.clone(),
                            signal_tx.clone(),
                            audio_playing.clone(),
                        ));
                    }
                }
            }
            SessionSignal::Stt(SttEvent::SpeechStarted) => {
                if speech_started_at.is_none() {
                    speech_started_at = Some(std::time::Instant::now());
                }
                let is_playing = audio_playing.load(std::sync::atomic::Ordering::SeqCst);
                let has_active_response = active_response.is_some();
                tracing::info!(
                    is_playing,
                    has_active_response,
                    "Voice session: STT SpeechStarted event received"
                );
                if let Some(task) = active_response.take() {
                    tracing::info!(
                        "STT: Speech started during active response, executing barge-in interruption"
                    );
                    task.abort();
                    output
                        .send(CallCommand::Clear)
                        .await
                        .map_err(|_| VoiceError::Protocol("call output closed".into()))?;

                    let was_speaking =
                        audio_playing.swap(false, std::sync::atomic::Ordering::SeqCst);
                    if let Some(interrupted) = current_active_prompt.take() {
                        if !was_speaking && !interrupted.starts_with("The call just connected") {
                            interrupted_prompt_buffer =
                                Some(match interrupted_prompt_buffer.take() {
                                    Some(prev) => format!("{prev} {interrupted}"),
                                    None => interrupted,
                                });
                        } else {
                            interrupted_prompt_buffer = None;
                        }
                    }
                }
                if let Some((transcript, timing)) = pending_transcripts.pop_front() {
                    response_number += 1;
                    current_active_prompt = Some(transcript.clone());
                    active_response = Some(spawn_response(
                        response_number,
                        context.clone(),
                        transcript,
                        Some(timing),
                        providers.agent.clone(),
                        providers.tts.clone(),
                        providers.jev.clone(),
                        output.clone(),
                        signal_tx.clone(),
                        audio_playing.clone(),
                    ));
                }
            }
            SessionSignal::Stt(SttEvent::FinalTranscript(mut transcript)) => {
                let now = std::time::Instant::now();
                let last_audio = last_audio_at.lock().unwrap().take();
                let timing = TurnTiming {
                    speech_started_at: speech_started_at.take(),
                    last_audio_at: last_audio,
                    transcript_received_at: now,
                };

                if let Some(prev) = interrupted_prompt_buffer.take() {
                    tracing::info!(
                        prev = %prev,
                        continuation = %transcript,
                        "Stitching fragmented speech turns from interrupted utterance"
                    );
                    transcript = format!("{prev} {transcript}");
                }

                let accumulator = speech_accumulator.clone();
                let sig = tokio::task::spawn_blocking(move || {
                    let mut acc = accumulator.lock().unwrap();
                    let signature = acc.extract_signature();
                    acc.clear();
                    signature
                })
                .await
                .ok()
                .flatten();

                if let Some(sig) = sig {
                    tracing::info!(
                        turn = response_number + 1,
                        "Biometrics: Extracted voice signature for speech turn"
                    );
                    context.voice_signature = Some(sig);
                } else {
                    tracing::debug!(
                        turn = response_number + 1,
                        "Biometrics: Insufficient audio for new signature, retaining previous voice signature"
                    );
                }

                if active_response.is_some() {
                    pending_transcripts.push_back((transcript, timing));
                } else {
                    response_number += 1;
                    current_active_prompt = Some(transcript.clone());
                    active_response = Some(spawn_response(
                        response_number,
                        context.clone(),
                        transcript,
                        Some(timing),
                        providers.agent.clone(),
                        providers.tts.clone(),
                        providers.jev.clone(),
                        output.clone(),
                        signal_tx.clone(),
                        audio_playing.clone(),
                    ));
                }
            }
            SessionSignal::ResponseFinished(number) => {
                if number == response_number {
                    active_response.take();
                    current_active_prompt = None;
                    interrupted_prompt_buffer = None;
                    audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
                    if let Some((transcript, timing)) = pending_transcripts.pop_front() {
                        response_number += 1;
                        current_active_prompt = Some(transcript.clone());
                        active_response = Some(spawn_response(
                            response_number,
                            context.clone(),
                            transcript,
                            Some(timing),
                            providers.agent.clone(),
                            providers.tts.clone(),
                            providers.jev.clone(),
                            output.clone(),
                            signal_tx.clone(),
                            audio_playing.clone(),
                        ));
                    }
                }
            }
            SessionSignal::Stop => break,
            SessionSignal::Failure(error) => {
                failure = Some(error);
                break;
            }
        }
    }

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

#[allow(clippy::too_many_arguments)]
fn spawn_response(
    number: u64,
    context: CallContext,
    transcript: String,
    timing: Option<TurnTiming>,
    agent: Arc<dyn crate::voice::provider::AgentProvider>,
    tts: Arc<dyn crate::voice::provider::TtsProvider>,
    jev: Option<Arc<crate::agents::BridgeJevClient>>,
    output: mpsc::Sender<CallCommand>,
    signal: mpsc::Sender<SessionSignal>,
    audio_playing: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = stream_response(
            number,
            &context,
            &transcript,
            timing,
            agent,
            tts,
            jev.as_deref(),
            output,
            audio_playing.clone(),
        )
        .await;
        audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
        if let Err(error) = result {
            tracing::warn!(provider_error = %error, "voice response failed");
        }
        let _ = signal.send(SessionSignal::ResponseFinished(number)).await;
    })
}

pub use crate::voice::filler::{detect_action_filler, play_filler, strip_leading_ack};

#[allow(clippy::too_many_arguments)]
async fn stream_response(
    number: u64,
    context: &CallContext,
    transcript: &str,
    timing: Option<TurnTiming>,
    agent: Arc<dyn crate::voice::provider::AgentProvider>,
    tts: Arc<dyn crate::voice::provider::TtsProvider>,
    jev: Option<&crate::agents::BridgeJevClient>,
    output: mpsc::Sender<CallCommand>,
    audio_playing: Arc<std::sync::atomic::AtomicBool>,
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

    // Fast conversational filler for action/lookup queries to eliminate dead air (<500ms TTFA)
    // Runs filler audio synthesis/playback in parallel with requesting the core LLM stream
    let mut filler_played = false;
    let (filler_result, text_stream_result) = if let Some(filler) =
        detect_action_filler(transcript, jev, number).await
    {
        tracing::info!(
            turn = number,
            filler = %filler,
            "Playing immediate conversational filler for tool / lookup query concurrently with LLM stream"
        );
        let filler_fut = play_filler(
            filler,
            tts.as_ref(),
            &output,
            &mut first_audio_sent_at,
            &audio_playing,
        );
        let stream_fut = tokio::time::timeout(
            Duration::from_secs(30),
            agent.respond_stream(context, transcript),
        );
        let (f_res, s_res) = tokio::join!(filler_fut, stream_fut);
        (Some((filler, f_res)), s_res)
    } else {
        let s_res = tokio::time::timeout(
            Duration::from_secs(30),
            agent.respond_stream(context, transcript),
        )
        .await;
        (None, s_res)
    };

    if let Some((filler, f_res)) = filler_result {
        let ttfb = f_res?;
        if first_tts_ttfb_ms.is_none() {
            first_tts_ttfb_ms = Some(ttfb);
        }
        if first_sentence_at.is_none() {
            first_sentence_at = Some(std::time::Instant::now());
            first_sentence_text = Some(filler.to_string());
        }
        sentence_count += 1;
        filler_played = true;
    }

    let mut text_stream =
        text_stream_result.map_err(|_| VoiceError::Timeout("agent response"))??;

    while let Some(chunk_result) = tokio::time::timeout(Duration::from_secs(30), text_stream.next())
        .await
        .map_err(|_| VoiceError::Timeout("agent stream"))?
    {
        let chunk = chunk_result?;
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
                filler_played = false;
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

    if first_audio_sent_at.is_some() {
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

    audio_playing.store(false, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

async fn play_sentence(
    sentence: &str,
    tts: &dyn crate::voice::provider::TtsProvider,
    output: &mpsc::Sender<CallCommand>,
    first_audio_tracker: &mut Option<std::time::Instant>,
    audio_playing: &std::sync::atomic::AtomicBool,
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
            .map(|s| t.transcript_received_at.duration_since(s).as_millis())
    });

    let stt_endpointing_ms = timing.as_ref().and_then(|t| {
        t.last_audio_at
            .map(|a| t.transcript_received_at.duration_since(a).as_millis())
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
        time_to_first_audio_ms = ?time_to_first_audio_ms,
        user_perceived_delay_ms = ?user_perceived_delay_ms,
        stt_speech_duration_ms = ?stt_speech_duration_ms,
        stt_endpointing_ms = ?stt_endpointing_ms,
        queue_wait_ms,
        llm_ttft_ms = ?llm_ttft_ms,
        llm_ttfs_ms = ?llm_ttfs_ms,
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
           • LLM Time to 1st Token (TTFT): {llm_ttft_str}\n\
           • LLM Time to 1st Sentence:     {llm_ttfs_str}\n\
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
                jev: None,
            },
            event_tx,
            stt_session,
            agent,
            tts,
        )
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
    async fn speech_interrupted_before_playback_stitches_fragments() {
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
        // First fragment emitted
        event_tx
            .send(SttEvent::FinalTranscript("Can you".into()))
            .await
            .unwrap();

        // User immediately continues speaking before any audio is sent
        event_tx.send(SttEvent::SpeechStarted).await.unwrap();
        let clear = tokio::time::timeout(std::time::Duration::from_millis(100), output_rx.recv())
            .await
            .unwrap();
        assert_eq!(clear, Some(CallCommand::Clear));

        tts.pending.store(false, Ordering::SeqCst);
        // Second fragment emitted
        event_tx
            .send(SttEvent::FinalTranscript("remind me of chess".into()))
            .await
            .unwrap();

        for _ in 0..3 {
            output_rx.recv().await.unwrap();
        }
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();

        // Verify the two fragments were stitched into one coherent prompt!
        assert_eq!(
            agent.transcripts.lock().await.as_slice(),
            &["Can you remind me of chess"]
        );
    }

    #[tokio::test]
    async fn finalized_turns_are_processed_serially() {
        let (providers, event_tx, _, agent, _) = providers();
        let gate = Arc::new(Notify::new());
        *agent.first_gate.lock().await = Some(gate.clone());
        let (input_tx, input_rx) = mpsc::channel(8);
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
        while agent.transcripts.lock().await.is_empty() {
            tokio::task::yield_now().await;
        }
        event_tx
            .send(SttEvent::FinalTranscript("second".into()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(agent.transcripts.lock().await.as_slice(), &["first"]);

        gate.notify_one();
        for _ in 0..6 {
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

    #[tokio::test]
    async fn test_detect_action_filler() {
        assert_eq!(
            detect_action_filler(
                "Search for the date and opponent of India's next cricket match",
                None,
                0,
            )
            .await,
            Some("Let me check that for you.")
        );
        assert_eq!(
            detect_action_filler("Look up the current stock price of Apple", None, 0).await,
            Some("Let me look that up.")
        );
        assert_eq!(
            detect_action_filler(
                "Find a special specialty coffee shop in Indiranagar",
                None,
                0
            )
            .await,
            Some("Checking that for you.")
        );
        assert_eq!(
            detect_action_filler("The call just connected. Greet the user.", None, 0).await,
            None
        );
        assert_eq!(detect_action_filler("Nope.", None, 0).await, None);
        assert_eq!(
            detect_action_filler("My name is Rahul.", None, 0).await,
            None
        );
        assert_eq!(detect_action_filler("Cool, thanks.", None, 0).await, None);
        assert_eq!(detect_action_filler("Nothing. Bye.", None, 0).await, None);
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
