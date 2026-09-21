use crate::voice::provider::{TtsProvider, VoiceError};
use bytes::Bytes;
use dashmap::DashMap;
use futures_util::StreamExt;
use std::sync::{Arc, LazyLock};

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
    output: &tokio::sync::mpsc::Sender<crate::voice::session::CallCommand>,
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
                .send(crate::voice::session::CallCommand::Media(chunk.clone()))
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
            .send(crate::voice::session::CallCommand::Media(
                chunk_bytes.clone(),
            ))
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
    let acks = [
        "on it.",
        "on it,",
        "on it!",
        "on it",
        "done.",
        "done,",
        "done!",
        "done",
        "got it.",
        "got it,",
        "got it!",
        "got it",
        "sure thing.",
        "sure thing,",
        "sure thing!",
        "sure thing",
        "sure.",
        "sure,",
        "sure!",
        "certainly.",
        "certainly,",
        "certainly!",
        "certainly",
        "right away.",
        "right away,",
        "one moment.",
        "one moment,",
        "no problem.",
        "no problem,",
        "alright.",
        "alright,",
        "all right.",
        "all right,",
        "i'll note that down.",
        "i've noted that down.",
        "i have noted that down.",
        "i'll add that to your tasks.",
        "i've added that to your tasks.",
        "i have added that to your tasks.",
        "i'll set that reminder.",
        "i've set that reminder.",
        "i have set that reminder.",
        "i'll put that on your calendar.",
        "i've checked your calendar.",
        "checking your calendar.",
        "checking that for you.",
        "let me check that for you.",
        "looking that up for you.",
        "let me look that up.",
        "looking that up now.",
        "logging that for you.",
        "i've logged that.",
        "i have logged that.",
        "i'll draft that message.",
        "i've drafted that message.",
        "noted.",
        "noted,",
        "logged.",
        "logged,",
        "added.",
        "added,",
    ];
    let lower = effective.to_ascii_lowercase();
    for ack in acks {
        if lower.starts_with(ack) {
            let remainder = effective[ack.len()..].trim_start();
            if remainder.chars().any(|c| c.is_alphabetic()) {
                return remainder;
            } else {
                return "";
            }
        }
    }
    sentence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_leading_ack_extended() {
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
            strip_leading_ack("I've noted that down. Your reminder to call mom is set for 5 PM."),
            "Your reminder to call mom is set for 5 PM."
        );
        assert_eq!(
            strip_leading_ack("Logging that for you. 450 rupees logged under lunch."),
            "450 rupees logged under lunch."
        );
        assert_eq!(
            strip_leading_ack("I've checked your calendar. You are free after 3 PM."),
            "You are free after 3 PM."
        );
        assert_eq!(
            strip_leading_ack("[thoughtful] Got it. Apple is trading at ."),
            "Apple is trading at ."
        );
        assert_eq!(
            strip_leading_ack("[thoughtful] Apple is trading at ."),
            "[thoughtful] Apple is trading at ."
        );
        assert_eq!(strip_leading_ack("[thoughtful] Got it."), "");
    }

    #[tokio::test]
    async fn test_filler_cache_instant_playback() {
        use crate::voice::provider::AudioStream;
        use async_trait::async_trait;
        use std::sync::atomic::AtomicBool;
        use tokio::sync::mpsc;

        struct MockTts;
        #[async_trait]
        impl TtsProvider for MockTts {
            async fn synthesize(&self, _text: &str) -> Result<AudioStream, VoiceError> {
                let chunks = vec![Ok(Bytes::from_static(&[0xaa, 0xbb]))];
                Ok(Box::pin(futures_util::stream::iter(chunks)))
            }
        }

        let phrase = "I'll note that down.";
        let tts = MockTts;
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let mut first_audio = None;
        let playing = AtomicBool::new(false);

        let ttfb1 = play_filler(phrase, &tts, &output_tx, &mut first_audio, &playing)
            .await
            .unwrap();
        assert!(first_audio.is_some());
        let cmd1 = output_rx.recv().await.unwrap();
        assert_eq!(
            cmd1,
            crate::voice::session::CallCommand::Media(Bytes::from_static(&[0xaa, 0xbb]))
        );

        let mut first_audio2 = None;
        let ttfb2 = play_filler(phrase, &tts, &output_tx, &mut first_audio2, &playing)
            .await
            .unwrap();
        assert!(first_audio2.is_some());
        assert!(ttfb2 <= ttfb1 + 5);
        let cmd2 = output_rx.recv().await.unwrap();
        assert_eq!(
            cmd2,
            crate::voice::session::CallCommand::Media(Bytes::from_static(&[0xaa, 0xbb]))
        );
    }
}
