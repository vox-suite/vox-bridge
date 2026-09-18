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
    context: CallContext,
    mut input: mpsc::Receiver<CallEvent>,
    output: mpsc::Sender<CallCommand>,
) -> Result<(), VoiceError> {
    let stt = providers.stt.connect().await?;
    let (signal_tx, mut signal_rx) = mpsc::channel(32);
    let input_stt = stt.clone();
    let input_signal = signal_tx.clone();

    let last_audio_at = Arc::new(std::sync::Mutex::new(None));
    let input_last_audio = last_audio_at.clone();

    let mut vad = crate::voice::vad::VoiceActivityDetector::new();
    let input_task = tokio::spawn(async move {
        while let Some(event) = input.recv().await {
            match event {
                CallEvent::Audio(audio) => {
                    *input_last_audio.lock().unwrap() = Some(std::time::Instant::now());
                    if vad.process_frame(&audio) == crate::voice::vad::VadEvent::SpeechStarted {
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

    if let Some(ref opening) = context.initiation_context {
        response_number += 1;
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
        ));
    }

    while let Some(signal) = signal_rx.recv().await {
        match signal {
            SessionSignal::SpeechDetected => {
                if active_response.is_some() {
                    tracing::info!("Local VAD: Voice detected, executing instant barge-in interruption");
                    if speech_started_at.is_none() {
                        speech_started_at = Some(std::time::Instant::now());
                    }
                    if let Some(task) = active_response.take() {
                        task.abort();
                        output
                            .send(CallCommand::Clear)
                            .await
                            .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
                    }
                    if let Some((transcript, timing)) = pending_transcripts.pop_front() {
                        response_number += 1;
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
                        ));
                    }
                }
            }
            SessionSignal::Stt(SttEvent::SpeechStarted) => {
                if speech_started_at.is_none() {
                    speech_started_at = Some(std::time::Instant::now());
                }
                if let Some(task) = active_response.take() {
                    task.abort();
                    output
                        .send(CallCommand::Clear)
                        .await
                        .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
                }
                if let Some((transcript, timing)) = pending_transcripts.pop_front() {
                    response_number += 1;
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
                    ));
                }
            }
            SessionSignal::Stt(SttEvent::FinalTranscript(transcript)) => {
                let now = std::time::Instant::now();
                let last_audio = last_audio_at.lock().unwrap().take();
                let timing = TurnTiming {
                    speech_started_at: speech_started_at.take(),
                    last_audio_at: last_audio,
                    transcript_received_at: now,
                };

                if active_response.is_some() {
                    pending_transcripts.push_back((transcript, timing));
                } else {
                    response_number += 1;
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
                    ));
                }
            }
            SessionSignal::ResponseFinished(number) => {
                if number == response_number {
                    active_response.take();
                    if let Some((transcript, timing)) = pending_transcripts.pop_front() {
                        response_number += 1;
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
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = stream_response(number, &context, &transcript, timing, agent, tts, jev.as_deref(), output).await;
        if let Err(error) = result {
            tracing::warn!(provider_error = %error, "voice response failed");
        }
        let _ = signal.send(SessionSignal::ResponseFinished(number)).await;
    })
}

/// Detects if an incoming transcript is a lookup, search, or multi-step action that benefits
/// from an immediate fast audio filler / verbal acknowledgment to eliminate telephony dead air.
pub async fn detect_action_filler(
    transcript: &str,
    jev: Option<&crate::agents::BridgeJevClient>,
) -> Option<&'static str> {
    let lower = transcript.to_ascii_lowercase();
    let trimmed = lower.trim();

    // Do not play fillers for greetings, short social pleasantries, or simple confirmations
    if trimmed.is_empty()
        || trimmed.starts_with("the call just connected")
        || trimmed == "hi"
        || trimmed == "hello"
        || trimmed == "hey"
        || trimmed == "nope"
        || trimmed == "nope."
        || trimmed == "yes"
        || trimmed == "yeah"
        || trimmed == "no"
        || trimmed == "ok"
        || trimmed == "okay"
        || trimmed == "cool"
        || trimmed == "thanks"
        || trimmed == "thank you"
        || trimmed == "cool, thanks"
        || trimmed == "cool, thanks."
        || trimmed == "nothing"
        || trimmed == "nothing. bye"
        || trimmed == "nothing. bye."
        || trimmed == "bye"
        || trimmed == "goodbye"
        || trimmed.starts_with("my name is")
        || trimmed.starts_with("i am ")
        || trimmed.starts_with("i'm ")
        || trimmed.starts_with("call me ")
    {
        return None;
    }

    // High-accuracy Jev Choice evaluation if available
    if let Some(client) = jev {
        let state = serde_json::json!({ "transcript": transcript });
        if let Ok((choice, confidence)) = client
            .choice(
                state,
                "Select the appropriate immediate spoken acknowledgment for this request.",
                &[
                    ("none", Some("Pure conversational question, greeting, acknowledgment, or question answerable without external lookups")),
                    ("search", Some("General web lookups, current facts, sports scores, stock prices, weather")),
                    ("navigation", Some("Finding places, cafes, coffee shops, driving time, distance, traffic, or directions")),
                    ("organizing", Some("Adding tasks, calendar events, saving notes, reminders, or scheduling")),
                ],
            )
            .await
        {
            if confidence >= 0.70 {
                return match choice.as_str() {
                    "search" => Some("Let me look that up for you."),
                    "navigation" => Some("Checking that for you."),
                    "organizing" => Some("I'll take care of that for you."),
                    _ => None,
                };
            }
        }
    }

    // Specific domain fillers
    if trimmed.contains("stock price") || trimmed.contains("stock") || trimmed.contains("trading at") {
        return Some("Let me look that up.");
    }
    if trimmed.contains("coffee")
        || trimmed.contains("drive there")
        || trimmed.contains("how long")
        || trimmed.contains("route")
        || trimmed.contains("traffic")
    {
        return Some("Checking that for you.");
    }
    if trimmed.contains("cricket")
        || trimmed.contains("search for")
        || trimmed.contains("search")
        || trimmed.contains("find")
    {
        return Some("Let me check that for you.");
    }

    let action_keywords = [
        "look up", "lookup", "check", "weather",
        "remind", "add a task", "add task", "log a", "create a",
        "schedule", "calculate", "tell me about",
    ];

    for kw in action_keywords {
        if trimmed.contains(kw) {
            return Some("Let me check that for you.");
        }
    }

    if trimmed.split_whitespace().count() >= 6
        && (trimmed.contains('?')
            || trimmed.starts_with("what ")
            || trimmed.starts_with("where ")
            || trimmed.starts_with("when ")
            || trimmed.starts_with("who ")
            || trimmed.starts_with("how "))
    {
        return Some("Let me check that for you.");
    }

    None
}

/// Strips duplicate leading conversational acknowledgments (e.g. "On it.", "Done.") from
/// the core LLM response if an immediate filler was already played aloud.
pub fn strip_leading_ack(sentence: &str) -> &str {
    let acks = [
        "on it.", "on it,", "on it!", "on it",
        "done.", "done,", "done!", "done",
        "got it.", "got it,", "got it!", "got it",
        "sure thing.", "sure thing,", "sure thing!",
        "sure.", "sure,", "sure!",
        "certainly.", "certainly,",
        "right away.", "right away,",
        "one moment.", "one moment,",
    ];
    let lower = sentence.to_ascii_lowercase();
    for ack in acks {
        if lower.starts_with(ack) {
            let remainder = sentence[ack.len()..].trim_start();
            if remainder.chars().any(|c| c.is_alphabetic()) {
                return remainder;
            } else {
                return "";
            }
        }
    }
    sentence
}

async fn stream_response(
    number: u64,
    context: &CallContext,
    transcript: &str,
    timing: Option<TurnTiming>,
    agent: Arc<dyn crate::voice::provider::AgentProvider>,
    tts: Arc<dyn crate::voice::provider::TtsProvider>,
    jev: Option<&crate::agents::BridgeJevClient>,
    output: mpsc::Sender<CallCommand>,
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
    let (filler_result, text_stream_result) = if let Some(filler) = detect_action_filler(transcript, jev).await {
        tracing::info!(
            turn = number,
            filler = %filler,
            "Playing immediate conversational filler for tool / lookup query concurrently with LLM stream"
        );
        let filler_fut = play_sentence(filler, tts.as_ref(), &output, &mut first_audio_sent_at);
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

    let mut text_stream = text_stream_result
        .map_err(|_| VoiceError::Timeout("agent response"))??;

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
            match play_sentence(trimmed, tts.as_ref(), &output, &mut first_audio_sent_at).await {
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
                match play_sentence(trimmed, tts.as_ref(), &output, &mut first_audio_sent_at).await {
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

    output
        .send(CallCommand::Mark(format!("response-{number}")))
        .await
        .map_err(|_| VoiceError::Protocol("call output closed".into()))?;

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

    let queue_wait_ms = turn_started_at
        .duration_since(transcript_rx_at)
        .as_millis();

    let llm_ttft_ms =
        first_token_at.map(|ft| ft.duration_since(llm_request_start).as_millis());
    let llm_ttfs_ms =
        first_sentence_at.map(|fs| fs.duration_since(llm_request_start).as_millis());
    let tts_ttfb_ms = first_tts_ttfb_ms;

    // Time from transcript received to first audio dispatched to Twilio (pipeline latency)
    let time_to_first_audio_ms = first_audio_sent_at
        .map(|fa| fa.duration_since(transcript_rx_at).as_millis());

    // Time from caller stopped speaking to first audio dispatched to Twilio (user perceived delay)
    let user_perceived_delay_ms = timing.as_ref().and_then(|t| {
        t.last_audio_at
            .and_then(|la| first_audio_sent_at.map(|fa| fa.duration_since(la).as_millis()))
    });

    let total_turn_duration_ms =
        all_audio_sent_at.duration_since(transcript_rx_at).as_millis();

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
            detect_action_filler("Search for the date and opponent of India's next cricket match", None).await,
            Some("Let me check that for you.")
        );
        assert_eq!(
            detect_action_filler("Look up the current stock price of Apple", None).await,
            Some("Let me look that up.")
        );
        assert_eq!(
            detect_action_filler("Find a special specialty coffee shop in Indiranagar", None).await,
            Some("Checking that for you.")
        );
        assert_eq!(detect_action_filler("The call just connected. Greet the user.", None).await, None);
        assert_eq!(detect_action_filler("Nope.", None).await, None);
        assert_eq!(detect_action_filler("My name is Rahul.", None).await, None);
        assert_eq!(detect_action_filler("Cool, thanks.", None).await, None);
        assert_eq!(detect_action_filler("Nothing. Bye.", None).await, None);
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
