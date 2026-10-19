pub const ACKNOWLEDGEMENTS: &[&str] = &[
    "Okay, let me check that for you.",
    "Let me look into that.",
    "I'll check that now.",
    "One moment while I look that up.",
    "Let me see what I can find.",
    "I'll pull up the details.",
    "Let me check the latest information.",
    "Give me a moment to check.",
    "Okay, I'm looking into it.",
    "Let me take a closer look.",
    "I'll look that up for you.",
    "Let me check the weather there.",
    "I'll check the latest forecast.",
    "Let me check the current conditions.",
    "Let me check those details.",
    "I'll look through that for you.",
    "Let me check what's available.",
    "I'll check the latest options.",
    "Let me look up that order.",
    "I'll check the order details.",
    "Let me check your account details.",
    "I'll look through your recent history.",
    "Let me work through that.",
    "Give me a moment to put that together.",
    "On it.",
    "Opening that now.",
    "Let me check your calendar.",
    "I'll look through your inbox.",
    "Let me pull up your spending.",
    "Let me look at your transactions.",
    "Searching the web for that.",
    "Let me find the latest on that.",
    "Let me check the route.",
    "Let me find that place.",
    "Let me find that song.",
    "Let me look at your projects.",
    "Let me look through your spaces.",
    "I'll set up that reminder.",
    "I'll draft that message.",
    "Setting that up now.",
];

type FillerCache = std::sync::Mutex<
    std::collections::HashMap<(usize, &'static str), std::sync::Arc<Vec<bytes::Bytes>>>,
>;

fn filler_cache() -> &'static FillerCache {
    static CACHE: std::sync::OnceLock<FillerCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub struct CachedFiller<'a>(pub &'a std::sync::Arc<dyn crate::providers::tts::TtsProvider>);

#[async_trait::async_trait]
impl crate::providers::tts::TtsProvider for CachedFiller<'_> {
    async fn synthesize(
        &self,
        text: &str,
    ) -> Result<crate::providers::tts::AudioStream, crate::voice::provider::VoiceError> {
        use futures_util::StreamExt;
        let Some(phrase) = ACKNOWLEDGEMENTS.iter().copied().find(|p| *p == text) else {
            return self.0.synthesize(text).await;
        };
        let key = (std::sync::Arc::as_ptr(self.0) as *const () as usize, phrase);
        if let Some(chunks) = filler_cache().lock().unwrap().get(&key).cloned() {
            let chunks: Vec<_> = chunks.iter().cloned().map(Ok).collect();
            return Ok(Box::pin(futures_util::stream::iter(chunks)));
        }
        let collected = std::sync::Arc::new(std::sync::Mutex::new(Some(Vec::new())));
        let seen = collected.clone();
        let audio = self.0.synthesize(text).await?.inspect(move |item| {
            let mut seen = seen.lock().unwrap();
            match item {
                Ok(chunk) => {
                    if let Some(chunks) = seen.as_mut() {
                        chunks.push(chunk.clone());
                    }
                }
                Err(_) => *seen = None,
            }
        });
        let store = futures_util::stream::once(async move {
            if let Some(chunks) = collected.lock().unwrap().take().filter(|c| !c.is_empty()) {
                filler_cache()
                    .lock()
                    .unwrap()
                    .insert(key, std::sync::Arc::new(chunks));
            }
        })
        .filter_map(|()| async { None });
        Ok(Box::pin(audio.chain(store)))
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
        "sure thing,",
        "sure thing!",
        "sure thing.",
        "on it,",
        "on it!",
        "on it.",
        "sure,",
        "sure!",
        "sure.",
        "certainly,",
        "certainly!",
        "certainly.",
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
        "no problem,",
        "no problem!",
        "no problem.",
        "got it,",
        "got it!",
        "got it.",
        "understands,",
        "understood,",
        "understood.",
        "understood!",
        "okay,",
        "okay.",
        "ok,",
        "ok.",
        "right,",
        "right.",
        "great,",
        "great!",
        "great.",
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
