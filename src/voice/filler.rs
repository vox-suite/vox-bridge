use crate::voice::provider::{TtsProvider, VoiceError};
use bytes::Bytes;
use dashmap::DashMap;
use futures_util::StreamExt;
use std::sync::{Arc, LazyLock};

/// Global in-memory cache for pre-rendered G.711 μ-law filler audio frames.
/// Once synthesized or pre-warmed, subsequent turns stream audio directly from RAM in <1ms.
pub static FILLER_CACHE: LazyLock<DashMap<&'static str, Vec<Bytes>>> = LazyLock::new(DashMap::new);

/// List of all standard static filler phrases used across domains for pre-warming.
pub const PREWARM_FILLERS: &[&str] = &[
    "On it.",
    "Sure thing, one moment.",
    "Taking care of that right now.",
    "I'll note that down.",
    "Adding that to your tasks.",
    "I'll set that reminder for you.",
    "Checking your calendar.",
    "I'll put that on your schedule.",
    "Logging that for you.",
    "Got it, saving that.",
    "Let me calculate that for you.",
    "Let me crunch the numbers.",
    "Checking that for you.",
    "Checking places nearby.",
    "Let me find the best route.",
    "Let me look that up.",
    "Checking the weather for you.",
    "Let me check that for you.",
    "Looking that up now.",
    "I'll draft that message.",
];

/// Pre-warms the Sarvam TTS HTTP connection and primes the in-memory audio cache.
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

/// Synthesizes or retrieves cached audio for a spoken filler phrase, streaming chunks to Twilio.
pub async fn play_filler(
    phrase: &'static str,
    tts: &dyn TtsProvider,
    output: &tokio::sync::mpsc::Sender<crate::voice::session::CallCommand>,
    first_audio_tracker: &mut Option<std::time::Instant>,
    audio_playing: &std::sync::atomic::AtomicBool,
) -> Result<u128, VoiceError> {
    let start = std::time::Instant::now();

    // 1. Instant cache hit from memory (0ms TTFA)
    if let Some(cached_chunks) = FILLER_CACHE.get(phrase) {
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
        return Ok(ttfb_ms);
    }

    // 2. Cache miss: synthesize via TTS provider and populate cache
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

    Ok(ttfb_ms)
}

/// Detects if an incoming transcript is an action, tool lookup, task, or research query
/// that benefits from an immediate spoken conversational acknowledgment.
///
/// Tailors the phrase to the specific task category and rotates phrasing naturally across turns.
pub async fn detect_action_filler(
    transcript: &str,
    jev: Option<&crate::agents::BridgeJevClient>,
    turn: u64,
) -> Option<&'static str> {
    let trimmed = transcript.trim().to_ascii_lowercase();

    // Skip short greetings, single-word confirmations, or introductory phrases
    if trimmed.is_empty()
        || trimmed.starts_with("the call just connected")
        || trimmed == "hi"
        || trimmed == "hello"
        || trimmed == "hey"
        || trimmed == "yes"
        || trimmed == "yeah"
        || trimmed == "no"
        || trimmed == "nope"
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
        || trimmed.starts_with("who are you")
        || trimmed.starts_with("what can you do")
    {
        return None;
    }

    let t = turn as usize;

    // 1. Reminders & Tasks
    if trimmed.contains("remind me")
        || trimmed.contains("add a task")
        || trimmed.contains("add task")
        || trimmed.contains("create a task")
        || trimmed.contains("new task")
        || trimmed.contains("todo")
        || trimmed.contains("remember to")
        || trimmed.contains("put on my list")
        || trimmed.contains("don't let me forget")
        || trimmed.contains("set a reminder")
        || trimmed.contains("remind")
    {
        const TASK_FILLERS: &[&str] = &[
            "I'll note that down.",
            "Adding that to your tasks.",
            "I'll set that reminder for you.",
        ];
        return Some(TASK_FILLERS[t % TASK_FILLERS.len()]);
    }

    // 2. Calendar & Scheduling
    if trimmed.contains("schedule")
        || trimmed.contains("reschedule")
        || trimmed.contains("book a")
        || trimmed.contains("set up a meeting")
        || trimmed.contains("put on my calendar")
        || trimmed.contains("calendar")
        || trimmed.contains("free at")
        || trimmed.contains("free on")
        || trimmed.contains("my availability")
    {
        const CALENDAR_FILLERS: &[&str] = &[
            "Checking your calendar.",
            "I'll put that on your schedule.",
            "Looking at your calendar now.",
        ];
        return Some(CALENDAR_FILLERS[t % CALENDAR_FILLERS.len()]);
    }

    // 3. Notes, Expenses & Logging
    if trimmed.contains("log an expense")
        || trimmed.contains("log a")
        || trimmed.contains("log my")
        || trimmed.contains("record")
        || trimmed.contains("save note")
        || trimmed.contains("save a note")
        || trimmed.contains("take a note")
        || trimmed.contains("write down")
        || trimmed.contains("spent")
        || trimmed.contains("track this")
    {
        const LOG_FILLERS: &[&str] = &[
            "Logging that for you.",
            "Got it, saving that.",
            "Taking note of that.",
        ];
        return Some(LOG_FILLERS[t % LOG_FILLERS.len()]);
    }

    // 4. Calculations & Math
    if trimmed.contains("calculate")
        || trimmed.contains("how much is")
        || trimmed.contains("convert")
        || trimmed.contains("multiply")
        || trimmed.contains("divide")
        || trimmed.contains("sum of")
        || trimmed.contains("compute")
    {
        const CALC_FILLERS: &[&str] = &[
            "Let me calculate that for you.",
            "Let me crunch the numbers.",
        ];
        return Some(CALC_FILLERS[t % CALC_FILLERS.len()]);
    }

    // 5. Places, Coffee, Restaurants & Navigation
    if trimmed.contains("coffee")
        || trimmed.contains("restaurant")
        || trimmed.contains("cafe")
        || trimmed.contains("places to")
        || trimmed.contains("closest")
        || trimmed.contains("nearby")
    {
        const PLACE_FILLERS: &[&str] = &[
            "Checking that for you.",
            "Checking places nearby.",
            "Looking that up for you.",
        ];
        return Some(PLACE_FILLERS[t % PLACE_FILLERS.len()]);
    }
    if trimmed.contains("drive there")
        || trimmed.contains("how long")
        || trimmed.contains("route")
        || trimmed.contains("traffic")
        || trimmed.contains("directions")
        || trimmed.contains("how far")
    {
        const NAV_FILLERS: &[&str] = &[
            "Checking that for you.",
            "Let me find the best route.",
            "Looking up directions for you.",
        ];
        return Some(NAV_FILLERS[t % NAV_FILLERS.len()]);
    }

    // 6. Stocks & Finance
    if trimmed.contains("stock price")
        || trimmed.contains("stock")
        || trimmed.contains("trading at")
    {
        const STOCK_FILLERS: &[&str] =
            &["Let me look that up.", "Checking the latest price for you."];
        return Some(STOCK_FILLERS[t % STOCK_FILLERS.len()]);
    }

    // 7. Weather
    if trimmed.contains("weather")
        || trimmed.contains("forecast")
        || trimmed.contains("temperature")
        || trimmed.contains("raining")
    {
        const WEATHER_FILLERS: &[&str] =
            &["Checking the weather for you.", "Looking up the forecast."];
        return Some(WEATHER_FILLERS[t % WEATHER_FILLERS.len()]);
    }

    // 8. Sports & Cricket
    if trimmed.contains("cricket") || trimmed.contains("match") || trimmed.contains("score") {
        const CRICKET_FILLERS: &[&str] = &[
            "Let me check that for you.",
            "Looking up the match details.",
        ];
        return Some(CRICKET_FILLERS[t % CRICKET_FILLERS.len()]);
    }

    // 9. Messages & Communication
    if trimmed.contains("send a message")
        || trimmed.contains("send message")
        || trimmed.contains("send a text")
        || trimmed.contains("send text")
        || trimmed.contains("whatsapp")
        || trimmed.contains("draft an email")
        || trimmed.contains("send an email")
    {
        const MSG_FILLERS: &[&str] = &["I'll draft that message.", "Preparing that for you."];
        return Some(MSG_FILLERS[t % MSG_FILLERS.len()]);
    }

    // 10. General Search & Info Lookup
    let search_keywords = [
        "look up",
        "lookup",
        "search for",
        "search",
        "find",
        "google",
        "tell me about",
        "find out",
        "check",
    ];
    for kw in search_keywords {
        if trimmed.contains(kw) {
            const SEARCH_FILLERS: &[&str] = &[
                "Let me check that for you.",
                "Looking that up now.",
                "Finding that out for you.",
            ];
            return Some(SEARCH_FILLERS[t % SEARCH_FILLERS.len()]);
        }
    }

    // 11. High-accuracy Jev Choice evaluation if available
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
                    ("tasks", Some("Adding tasks, calendar events, saving notes, reminders, or scheduling")),
                ],
            )
            .await
        {
            match choice.as_str() {
                "search" if confidence >= 0.70 => return Some("Looking that up now."),
                "navigation" if confidence >= 0.70 => return Some("Checking that for you."),
                "tasks" if confidence >= 0.70 => return Some("I'll note that down."),
                _ => {}
            }
        }
    }

    // 12. General Action / Complex Multi-step questions
    if trimmed.split_whitespace().count() >= 6
        && (trimmed.contains('?')
            || trimmed.starts_with("what ")
            || trimmed.starts_with("where ")
            || trimmed.starts_with("when ")
            || trimmed.starts_with("who ")
            || trimmed.starts_with("how ")
            || trimmed.starts_with("can you ")
            || trimmed.starts_with("could you "))
    {
        const ACTION_FILLERS: &[&str] = &[
            "On it.",
            "Taking care of that right now.",
            "Sure thing, one moment.",
        ];
        return Some(ACTION_FILLERS[t % ACTION_FILLERS.len()]);
    }

    None
}

/// Strips duplicate leading conversational acknowledgments from
/// the core LLM response if an immediate filler was already played aloud.
pub fn strip_leading_ack(sentence: &str) -> &str {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_task_fillers_by_domain() {
        // Reminders & tasks
        assert_eq!(
            detect_action_filler("Remind me to call mom at 5 PM", None, 0).await,
            Some("I'll note that down.")
        );
        assert_eq!(
            detect_action_filler("Add a task to buy groceries", None, 1).await,
            Some("Adding that to your tasks.")
        );
        assert_eq!(
            detect_action_filler(
                "Please set a reminder for the doctor's appointment",
                None,
                2
            )
            .await,
            Some("I'll set that reminder for you.")
        );

        // Calendar
        assert_eq!(
            detect_action_filler("Schedule a meeting with John tomorrow at 3 PM", None, 0).await,
            Some("Checking your calendar.")
        );
        assert_eq!(
            detect_action_filler("Put dentist appointment on my calendar", None, 1).await,
            Some("I'll put that on your schedule.")
        );

        // Notes & Logging
        assert_eq!(
            detect_action_filler("Log an expense of 450 rupees for lunch", None, 0).await,
            Some("Logging that for you.")
        );
        assert_eq!(
            detect_action_filler("Save note that the door code is 1234", None, 1).await,
            Some("Got it, saving that.")
        );

        // Calculations
        assert_eq!(
            detect_action_filler("Calculate 18 percent of 3500", None, 0).await,
            Some("Let me calculate that for you.")
        );
        assert_eq!(
            detect_action_filler("How much is 45 times 68", None, 1).await,
            Some("Let me crunch the numbers.")
        );

        // Places & Navigation
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
            detect_action_filler(
                "Find a special specialty coffee shop in Indiranagar",
                None,
                1
            )
            .await,
            Some("Checking places nearby.")
        );
        assert_eq!(
            detect_action_filler("How is the traffic to the airport right now?", None, 0).await,
            Some("Checking that for you.")
        );

        // Stocks
        assert_eq!(
            detect_action_filler("Look up the current stock price of Apple", None, 0).await,
            Some("Let me look that up.")
        );
        assert_eq!(
            detect_action_filler("Look up the current stock price of Apple", None, 1).await,
            Some("Checking the latest price for you.")
        );

        // Weather
        assert_eq!(
            detect_action_filler("What is the weather forecast for tomorrow?", None, 0).await,
            Some("Checking the weather for you.")
        );

        // Cricket
        assert_eq!(
            detect_action_filler(
                "Search for the date and opponent of India's next cricket match",
                None,
                0
            )
            .await,
            Some("Let me check that for you.")
        );

        // Messaging
        assert_eq!(
            detect_action_filler(
                "Send a WhatsApp message to Rahul saying I'll be late",
                None,
                0
            )
            .await,
            Some("I'll draft that message.")
        );

        // Negative tests / Chitchat
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
        assert_eq!(detect_action_filler("Who are you?", None, 0).await, None);
    }

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

        // First call: populates cache
        let ttfb1 = play_filler(phrase, &tts, &output_tx, &mut first_audio, &playing)
            .await
            .unwrap();
        assert!(first_audio.is_some());
        let cmd1 = output_rx.recv().await.unwrap();
        assert_eq!(
            cmd1,
            crate::voice::session::CallCommand::Media(Bytes::from_static(&[0xaa, 0xbb]))
        );

        // Second call: instant cache hit (<5ms)
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
