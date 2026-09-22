// this file code contains agent response streaming and tts synthesis

use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::channels::context::CallContext;
use crate::core::{ConversationClient, ConversationEvent};
use crate::providers::tts::TtsProvider;
use crate::voice::chunker::SentenceChunker;
use crate::voice::filler::{play_filler, strip_leading_ack};
use crate::voice::metrics::{TurnTiming, log_turn_latency};
use crate::voice::provider::VoiceError;
use crate::voice::session::mod_types::{CallCommand, SessionSignal};

struct ResponseLifecycle {
    number: u64,
    conversation_id: String,
    started: std::time::Instant,
    outcome: &'static str,
}

impl Drop for ResponseLifecycle {
    fn drop(&mut self) {
        tracing::info!(
            turn = self.number,
            conversation_id = %self.conversation_id,
            outcome = self.outcome,
            elapsed_ms = self.started.elapsed().as_millis(),
            "VOICE_RESPONSE_ENDED"
        );
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

#[allow(clippy::too_many_arguments)]
async fn stream_response(
    number: u64,
    context: &CallContext,
    transcript: &str,
    timing: Option<TurnTiming>,
    agent: Arc<dyn ConversationClient>,
    tts: Arc<dyn TtsProvider>,
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

    let mut chunker = SentenceChunker::new();
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
    let mut text_stream = tokio::time::timeout(
        Duration::from_secs(30),
        agent.respond_events(context, transcript),
    )
    .await
    .map_err(|_| VoiceError::Timeout("Core response"))??;

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
        let Some(event) = next else {
            break;
        };
        let chunk = match event? {
            ConversationEvent::LookupPending => {
                if !filler_played && first_token_at.is_none() && lookup_deadline.is_none() {
                    lookup_deadline =
                        Some(tokio::time::Instant::now() + Duration::from_millis(400));
                }
                continue;
            }
            ConversationEvent::Text(text) => {
                if text.trim().is_empty() {
                    continue;
                }
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
    tts: &dyn TtsProvider,
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
