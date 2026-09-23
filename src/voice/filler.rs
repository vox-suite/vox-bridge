/**
* this file code contains conversational filler generation and caching
*/
use bytes::Bytes;
use dashmap::DashMap;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

use crate::providers::tts::TtsProvider;
use crate::voice::provider::VoiceError;
use crate::voice::session::CallCommand;

pub static FILLER_CACHE: LazyLock<DashMap<&'static str, Vec<Bytes>>> = LazyLock::new(DashMap::new);
static FILLER_ROTATION: AtomicUsize = AtomicUsize::new(0);

pub const FILLER_LOOKING_INTO_THAT: &str = "I'm looking into that.";
pub const FILLER_CHECK_ORDER: &str = "Let me check on your order.";
pub const FILLER_CHECK_ACCOUNT: &str = "Let me look up your account details.";
pub const FILLER_CHECK_INVENTORY: &str = "Let me check the latest availability for you.";
pub const FILLER_GIVE_ME_A_MOMENT: &str = "Give me just a moment to pull that together.";

pub const GENERAL_FILLERS: &[&str] = &[
    "I'm looking into that.",
    "Let me check on that for you.",
    "Let me pull that up.",
    "One moment, looking into that.",
    "Checking on that right now.",
    "Let me see what I can find.",
];

pub const ACTION_FILLERS: &[&str] = &[
    "Give me just a moment to pull that together.",
    "Working on that for you right now.",
    "Let me take care of that for you.",
    "Putting that together now.",
    "On it, give me just a second.",
    "Getting that set up for you.",
];

pub const ACCOUNT_FILLERS: &[&str] = &[
    "Let me look up your account details.",
    "Let me check our recent conversation.",
    "Checking your notes and history now.",
    "Let me pull up your account records.",
    "Looking back at what we discussed.",
];

pub const ORDER_FILLERS: &[&str] = &[
    "Let me check on your order.",
    "Looking up that order for you now.",
    "Checking the tracking details on that.",
    "Let me pull up the status of your order.",
];

pub const INVENTORY_FILLERS: &[&str] = &[
    "Let me check the latest availability for you.",
    "Looking up the latest options for you now.",
    "Checking availability right now.",
    "Let me see what's currently available.",
];

// Tone-specific fillers: Empathetic (for customer issues, delays, frustration)
pub const EMPATHETIC_ORDER_FILLERS: &[&str] = &[
    "I understand, let me check on your order right away.",
    "Let me look into what happened with your order.",
    "I hear you, checking your order details right now.",
];

pub const EMPATHETIC_ACCOUNT_FILLERS: &[&str] = &[
    "I understand, let me look into your account details.",
    "Let me pull up your account records right away.",
    "Checking your account details now to get this sorted.",
];

pub const EMPATHETIC_ACTION_FILLERS: &[&str] = &[
    "I understand, give me just a moment to sort this out for you.",
    "On it, let me get this taken care of for you right now.",
    "Let me take care of that for you right away.",
];

pub const EMPATHETIC_GENERAL_FILLERS: &[&str] = &[
    "I understand, let me look into that for you right now.",
    "I hear you, let me check on that right away.",
    "Give me just a moment, I'm checking into this now.",
];

// Tone-specific fillers: Enthusiastic (for upbeat, positive, excited callers)
pub const ENTHUSIASTIC_ORDER_FILLERS: &[&str] = &[
    "Sure thing! Let me pull up your order details.",
    "I'd love to check on your order for you!",
    "Great, let me pull up the status of your order!",
];

pub const ENTHUSIASTIC_ACCOUNT_FILLERS: &[&str] = &[
    "Sure thing! Let me pull up your account details.",
    "Awesome, checking your account history right now!",
    "I'd be glad to look up your account records!",
];

pub const ENTHUSIASTIC_INVENTORY_FILLERS: &[&str] = &[
    "Great question! Let me check the latest availability for you.",
    "Let me find the best options available for you right now!",
    "Checking availability for you right away!",
];

pub const ENTHUSIASTIC_ACTION_FILLERS: &[&str] = &[
    "Absolutely! Give me just a second to pull that together.",
    "You got it! Getting that set up for you right now.",
    "I'm on it! Let me take care of that for you.",
];

pub const ENTHUSIASTIC_GENERAL_FILLERS: &[&str] = &[
    "Sure thing, let me find that for you!",
    "I'm on it! Let me check that right away.",
    "Awesome, let me pull that up for you!",
];

// Essential fillers to prewarm safely without triggering 429 concurrency limit
pub const PREWARM_FILLERS: &[&str] = &[
    FILLER_LOOKING_INTO_THAT,
    FILLER_CHECK_ORDER,
    FILLER_CHECK_ACCOUNT,
    FILLER_CHECK_INVENTORY,
    FILLER_GIVE_ME_A_MOMENT,
];

pub fn filler_for_choice_index(choice: &str, idx: usize) -> &'static str {
    match choice {
        "pleasantry" | "gratitude" | "none" => "",
        "check_order" | "orders_shipping" => ORDER_FILLERS[idx % ORDER_FILLERS.len()],
        "check_account" | "account_history" => ACCOUNT_FILLERS[idx % ACCOUNT_FILLERS.len()],
        "check_inventory" | "inventory_availability" => {
            INVENTORY_FILLERS[idx % INVENTORY_FILLERS.len()]
        }
        "give_me_a_moment" | "actions_tasks" => ACTION_FILLERS[idx % ACTION_FILLERS.len()],
        _ => GENERAL_FILLERS[idx % GENERAL_FILLERS.len()],
    }
}

pub fn filler_for_choice(choice: &str) -> &'static str {
    filler_for_choice_index(choice, 0)
}

pub fn rotate_filler_for_choice(choice: &str) -> &'static str {
    let idx = FILLER_ROTATION.fetch_add(1, Ordering::Relaxed);
    filler_for_choice_index(choice, idx)
}

pub fn rotate_filler_for_choice_and_tone(choice: &str, tone: &str) -> &'static str {
    let idx = FILLER_ROTATION.fetch_add(1, Ordering::Relaxed);
    match (choice, tone) {
        ("pleasantry" | "gratitude" | "none", _) => "",
        ("check_order" | "orders_shipping", "empathetic") => {
            EMPATHETIC_ORDER_FILLERS[idx % EMPATHETIC_ORDER_FILLERS.len()]
        }
        ("check_order" | "orders_shipping", "enthusiastic") => {
            ENTHUSIASTIC_ORDER_FILLERS[idx % ENTHUSIASTIC_ORDER_FILLERS.len()]
        }
        ("check_order" | "orders_shipping", _) => ORDER_FILLERS[idx % ORDER_FILLERS.len()],

        ("check_account" | "account_history", "empathetic") => {
            EMPATHETIC_ACCOUNT_FILLERS[idx % EMPATHETIC_ACCOUNT_FILLERS.len()]
        }
        ("check_account" | "account_history", "enthusiastic") => {
            ENTHUSIASTIC_ACCOUNT_FILLERS[idx % ENTHUSIASTIC_ACCOUNT_FILLERS.len()]
        }
        ("check_account" | "account_history", _) => ACCOUNT_FILLERS[idx % ACCOUNT_FILLERS.len()],

        ("check_inventory" | "inventory_availability", "enthusiastic") => {
            ENTHUSIASTIC_INVENTORY_FILLERS[idx % ENTHUSIASTIC_INVENTORY_FILLERS.len()]
        }
        ("check_inventory" | "inventory_availability", _) => {
            INVENTORY_FILLERS[idx % INVENTORY_FILLERS.len()]
        }

        ("give_me_a_moment" | "actions_tasks", "empathetic") => {
            EMPATHETIC_ACTION_FILLERS[idx % EMPATHETIC_ACTION_FILLERS.len()]
        }
        ("give_me_a_moment" | "actions_tasks", "enthusiastic") => {
            ENTHUSIASTIC_ACTION_FILLERS[idx % ENTHUSIASTIC_ACTION_FILLERS.len()]
        }
        ("give_me_a_moment" | "actions_tasks", _) => ACTION_FILLERS[idx % ACTION_FILLERS.len()],

        (_, "empathetic") => EMPATHETIC_GENERAL_FILLERS[idx % EMPATHETIC_GENERAL_FILLERS.len()],
        (_, "enthusiastic") => {
            ENTHUSIASTIC_GENERAL_FILLERS[idx % ENTHUSIASTIC_GENERAL_FILLERS.len()]
        }
        (_, _) => GENERAL_FILLERS[idx % GENERAL_FILLERS.len()],
    }
}

pub fn is_conversational_pleasantry(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    let cleaned = lower
        .replace(['.', ',', '!', '?', '-', '—', ';', ':', '\'', '"'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if cleaned.is_empty() {
        return false;
    }

    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.len() > 6 {
        return false;
    }

    // Interrogative or task action words imply an active request, not just a pleasantry.
    let interrogatives = [
        "what", "whats", "when", "where", "who", "why", "how", "can", "could", "would", "will",
        "create", "schedule", "check", "tell", "find", "search", "show", "open", "call", "do",
        "did",
    ];
    if words.iter().any(|w| interrogatives.contains(w)) {
        return false;
    }

    matches!(
        cleaned.as_str(),
        "thanks"
            | "thank you"
            | "thank you very much"
            | "thanks a lot"
            | "thanks so much"
            | "thank you so much"
            | "cool thanks"
            | "good thanks"
            | "great thanks"
            | "awesome thanks"
            | "perfect thanks"
            | "ok thanks"
            | "okay thanks"
            | "alright thanks"
            | "all right thanks"
            | "sounds good"
            | "sounds great"
            | "sounds good thanks"
            | "sounds great thanks"
            | "got it thanks"
            | "got it thank you"
            | "all good"
            | "all set"
            | "that is all"
            | "thats all"
            | "thats all thanks"
            | "thats it"
            | "thats it thanks"
            | "bye"
            | "goodbye"
            | "bye bye"
            | "have a good day"
            | "have a great day"
            | "have a nice day"
            | "take care"
            | "no thats all"
            | "no thats it"
            | "no that is all"
            | "nope thats all"
            | "no problem"
            | "youre welcome"
            | "cool"
            | "awesome"
            | "perfect"
            | "great"
            | "nice"
            | "sweet"
    ) || cleaned.starts_with("thank")
        || cleaned.starts_with("bye")
        || cleaned.ends_with("thanks")
        || cleaned.ends_with("thank you")
        || cleaned.ends_with("great day")
        || cleaned.ends_with("good day")
        || cleaned.ends_with("nice day")
}

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
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        tracing::info!(
            cached_count = FILLER_CACHE.len(),
            "Pre-warmed filler audio cache safely"
        );
    });
}

pub async fn play_filler(
    phrase: &'static str,
    tts: &dyn TtsProvider,
    output: &tokio::sync::mpsc::Sender<CallCommand>,
    first_audio_tracker: &mut Option<std::time::Instant>,
    audio_playing: &std::sync::atomic::AtomicBool,
    generation: u64,
) -> Result<u128, VoiceError> {
    let start = std::time::Instant::now();

    let cached = FILLER_CACHE.get(phrase).map(|entry| entry.value().clone());
    if let Some(cached_chunks) = cached {
        let mut ttfb_ms = 0;
        let mut is_first = true;
        for chunk in cached_chunks.iter() {
            output
                .send(CallCommand::Media {
                    bytes: chunk.clone(),
                    generation,
                })
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
            phrase = phrase,
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
            .send(CallCommand::Media {
                bytes: chunk_bytes.clone(),
                generation,
            })
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
        phrase = phrase,
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
        "absolutely,",
        "absolutely!",
        "you got it,",
        "you got it!",
        "i understand,",
        "i understand.",
        "i hear you,",
        "let me check that for you.",
        "let me look that up for you.",
        "let me check that.",
        "let me check,",
        "let me check on your order.",
        "looking up that order for you now.",
        "let me look up your account details.",
        "let me check our recent conversation.",
        "let me check the latest availability for you.",
        "give me just a moment to pull that together.",
        "working on that for you right now.",
        "let me take care of that for you.",
        "putting that together now.",
        "i'm looking into that.",
        "let me see,",
        "let me see.",
        "i understand, let me check on your order right away.",
        "let me look into what happened with your order.",
        "i understand, let me look into your account details.",
        "let me pull up your account records right away.",
        "i understand, give me just a moment to sort this out for you.",
        "i understand, let me look into that for you right now.",
        "sure thing! let me pull up your order details.",
        "i'd love to check on your order for you!",
        "sure thing! let me pull up your account details.",
        "awesome, checking your account history right now!",
        "great question! let me check the latest availability for you.",
        "absolutely! give me just a second to pull that together.",
        "you got it! getting that set up for you right now.",
        "sure thing, let me find that for you!",
        "i'm on it! let me check that right away.",
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
