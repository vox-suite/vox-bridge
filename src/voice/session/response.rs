/**
* this file code contains agent response streaming and tts synthesis
*/
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::channels::context::CallContext;
use crate::core::{ConversationClient, ConversationEvent, ConversationEventStream};
use crate::providers::jev::JevClient;
use crate::providers::tts::TtsProvider;
use crate::voice::chunker::SentenceChunker;
use crate::voice::filler::{
    is_conversational_pleasantry, play_filler, rotate_filler_for_choice_and_tone, strip_leading_ack,
};
use crate::voice::metrics::{TurnTiming, log_turn_latency};
use crate::voice::provider::VoiceError;
use crate::voice::session::mod_types::{CallCommand, SessionSignal};
use crate::voice::session::playback::PlaybackState;

enum Spoken {
    Filler(&'static str),
    Sentence(String),
    Mark(String),
}

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
    filler_tts: Arc<dyn TtsProvider>,
    jev: Option<Arc<JevClient>>,
    output: mpsc::Sender<CallCommand>,
    signal: mpsc::Sender<SessionSignal>,
    audio_playing: Arc<std::sync::atomic::AtomicBool>,
    answer_started: Arc<std::sync::atomic::AtomicBool>,
    playback: Arc<PlaybackState>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut lifecycle = ResponseLifecycle {
            number,
            conversation_id: context.external_conversation_id.clone(),
            started: std::time::Instant::now(),
            outcome: "cancelled",
        };
        let apology_tts = tts.clone();
        let apology_output = output.clone();
        let apology_answer_started = answer_started.clone();
        let apology_playback = playback.clone();
        let result = stream_response(
            number,
            &context,
            &transcript,
            timing,
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
                lifecycle.outcome = "completed";
                text.clone()
            }
            Err(error) => {
                lifecycle.outcome = "failed";
                tracing::warn!(provider_error = %error, "voice response failed");
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
    filler_tts: Arc<dyn TtsProvider>,
    jev: Option<Arc<JevClient>>,
    output: mpsc::Sender<CallCommand>,
    audio_playing: Arc<std::sync::atomic::AtomicBool>,
    answer_started: Arc<std::sync::atomic::AtomicBool>,
    playback: Arc<PlaybackState>,
) -> Result<String, VoiceError> {
    let turn_started_at = std::time::Instant::now();
    let llm_request_start = std::time::Instant::now();

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
    let response_mark = format!("response-{number}");
    let player = tokio::spawn(async move {
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
                Spoken::Sentence(sentence) => {
                    match play_sentence(
                        &sentence,
                        player_tts.as_ref(),
                        &player_output,
                        &mut first_audio_sent_at,
                        &player_audio,
                        &player_answer,
                        &player_playback,
                        generation,
                    )
                    .await
                    {
                        Ok(ttfb) => {
                            if first_tts_ttfb_ms.is_none() {
                                first_tts_ttfb_ms = Some(ttfb);
                            }
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, sentence = %sentence, "TTS synthesis failed for sentence, continuing turn");
                        }
                    }
                }
                Spoken::Mark(name) => {
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
    });

    let mut chunker = SentenceChunker::new();
    let mut first_token_at: Option<std::time::Instant> = None;
    let mut first_sentence_at: Option<std::time::Instant> = None;
    let mut first_sentence_text: Option<String> = None;
    let mut first_audio_sent_at: Option<std::time::Instant> = None;
    let mut first_tts_ttfb_ms: Option<u128> = None;
    let mut full_response = String::new();
    let mut sentence_count = 0usize;

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
        let chunk = match event? {
            ConversationEvent::LookupPending => {
                continue;
            }
            ConversationEvent::Text(text) => {
                if text.trim().is_empty() {
                    continue;
                }
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
            spoken_tx
                .send(Spoken::Sentence(trimmed.to_string()))
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
                if sentence_count == 1 || first_sentence_at.is_none() {
                    first_sentence_at = Some(std::time::Instant::now());
                    first_sentence_text = Some(trimmed.to_string());
                }
                spoken_tx
                    .send(Spoken::Sentence(trimmed.to_string()))
                    .await
                    .map_err(|_| VoiceError::Protocol("speech output closed".into()))?;
            }
        }
    }

    drop(spoken_tx);
    if let Ok((played_ttfb, played_audio, _)) = player.await {
        first_tts_ttfb_ms = played_ttfb;
        first_audio_sent_at = played_audio;
    }

    let all_audio_sent_at = std::time::Instant::now();

    log_turn_latency(
        number,
        &mut_context,
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
        VoiceError::Timeout(_) => "Sorry, that's taking longer than expected. Could you say that again?",
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
        if !playback.accepts(generation) {
            break;
        }
        let chunk_bytes = chunk?;
        output
            .send(CallCommand::Media {
                bytes: chunk_bytes,
                generation,
            })
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
