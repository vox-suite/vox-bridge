// this file code contains conversational filler generation and caching

use bytes::Bytes;
use dashmap::DashMap;
use futures_util::StreamExt;
use std::sync::{Arc, LazyLock};

use crate::providers::tts::TtsProvider;
use crate::voice::provider::VoiceError;
use crate::voice::session::CallCommand;

pub static FILLER_CACHE: LazyLock<DashMap<&'static str, Vec<Bytes>>> = LazyLock::new(DashMap::new);

pub const PREWARM_FILLERS: &[&str] = &["I'm looking into that."];

pub fn prewarm_fillers(tts: Arc<dyn TtsProvider>) {
    tokio::spawn(async move {
        for &phrase in PREWARM_FILLERS {
            if FILLER_CACHE.contains_key(phrase) {
                continue;
            }
            if let Ok(mut stream) = tts.synthesize(phrase).await {
                let mut collected = Vec::new();
                while let Some(Ok(chunk)) = stream.next().await {
                    collected.push(chunk);
                }
                if !collected.is_empty() {
                    FILLER_CACHE.insert(phrase, collected);
                }
            }
        }
        tracing::info!(
            cached_count = FILLER_CACHE.len(),
            "Pre-warmed filler audio cache"
        );
    });
}

pub async fn play_filler(
    phrase: &'static str,
    tts: &dyn TtsProvider,
    output: &tokio::sync::mpsc::Sender<CallCommand>,
    first_audio_tracker: &mut Option<std::time::Instant>,
    audio_playing: &std::sync::atomic::AtomicBool,
) -> Result<u128, VoiceError> {
    let start = std::time::Instant::now();

    let cached = FILLER_CACHE.get(phrase).map(|entry| entry.value().clone());
    if let Some(cached_chunks) = cached {
        let mut ttfb_ms = 0;
        let mut is_first = true;
        for chunk in cached_chunks.iter() {
            output
                .send(CallCommand::Media(chunk.clone()))
                .await
                .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
            audio_playing.store(true, std::sync::atomic::Ordering::SeqCst);
            if is_first {
                ttfb_ms = start.elapsed().as_millis();
                if first_audio_tracker.is_none() {
                    *first_audio_tracker = Some(std::time::Instant::now());
                }
                is_first = false;
            }
        }
        tracing::info!(
            cached = true,
            elapsed_ms = start.elapsed().as_millis(),
            "FILLER_AUDIO_ENQUEUED"
        );
        return Ok(ttfb_ms);
    }

    let text = phrase.trim();
    if text.is_empty() || !text.chars().any(|c| c.is_alphabetic()) {
        return Ok(0);
    }

    let mut audio = tts.synthesize(text).await?;
    let mut ttfb_ms = 0;
    let mut is_first = true;
    let mut collected = Vec::new();

    while let Some(chunk) = tokio::time::timeout(std::time::Duration::from_secs(10), audio.next())
        .await
        .map_err(|_| VoiceError::Timeout("TTS audio"))?
    {
        let chunk_bytes = chunk?;
        output
            .send(CallCommand::Media(chunk_bytes.clone()))
            .await
            .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
        audio_playing.store(true, std::sync::atomic::Ordering::SeqCst);

        let now = std::time::Instant::now();
        if is_first {
            ttfb_ms = now.duration_since(start).as_millis();
            if first_audio_tracker.is_none() {
                *first_audio_tracker = Some(now);
            }
            is_first = false;
        }
        collected.push(chunk_bytes);
    }

    if !collected.is_empty() {
        FILLER_CACHE.insert(phrase, collected);
    }

    tracing::info!(
        cached = false,
        elapsed_ms = start.elapsed().as_millis(),
        "FILLER_AUDIO_ENQUEUED"
    );
    Ok(ttfb_ms)
}

pub fn strip_leading_ack(sentence: &str) -> &str {
    let trimmed = sentence.trim_start();
    let effective = if trimmed.starts_with('[') {
        if let Some(idx) = trimmed.find(']') {
            trimmed[idx + 1..].trim_start()
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    let lower = effective.to_ascii_lowercase();
    let ack_prefixes = [
        "sure,",
        "sure!",
        "sure.",
        "sure ",
        "certainly,",
        "certainly!",
        "certainly.",
        "certainly ",
        "i'd be happy to help with that.",
        "i'd be happy to help with that,",
        "i'd be happy to help.",
        "i'd be happy to help,",
        "i can help with that.",
        "i can help with that,",
        "i can certainly help with that.",
        "i can certainly help with that,",
        "of course,",
        "of course!",
        "of course.",
        "of course ",
        "no problem,",
        "no problem!",
        "no problem.",
        "no problem ",
        "got it,",
        "got it!",
        "got it.",
        "got it ",
        "understands,",
        "understood,",
        "understood.",
        "understood!",
        "understood ",
        "okay,",
        "okay.",
        "okay ",
        "ok,",
        "ok.",
        "ok ",
        "right,",
        "right.",
        "right ",
        "great,",
        "great!",
        "great.",
        "great ",
        "let me check that for you.",
        "let me look that up for you.",
        "let me check that.",
        "let me check,",
        "let me see,",
        "let me see.",
    ];

    for prefix in &ack_prefixes {
        if lower.starts_with(prefix) {
            let stripped = effective[prefix.len()..].trim_start();
            if !stripped.is_empty() && stripped.chars().any(|c| c.is_alphabetic()) {
                return stripped;
            }
        }
    }

    effective
}
