#[derive(Debug, Default)]
pub struct SentenceChunker {
    buffer: String,
}

impl SentenceChunker {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Appends a new text chunk to the buffer and extracts any completed sentences.
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        self.drain_sentences(false)
    }

    /// Flushes any remaining uncompleted text in the buffer as a final sentence.
    pub fn flush(&mut self) -> Option<String> {
        let remaining = self.buffer.trim().to_string();
        self.buffer.clear();
        if remaining.is_empty() || !remaining.chars().any(|c| c.is_alphabetic()) {
            None
        } else {
            Some(remaining)
        }
    }

    fn drain_sentences(&mut self, flush: bool) -> Vec<String> {
        let mut sentences = Vec::new();
        while let Some(index) = self.find_sentence_boundary() {
            let sentence = self.buffer[..index].trim().to_string();
            self.buffer = self.buffer[index..].trim_start().to_string();
            if !sentence.is_empty() && sentence.chars().any(|c| c.is_alphabetic()) {
                sentences.push(sentence);
            }
        }
        if flush {
            if let Some(final_sentence) = self.flush() {
                sentences.push(final_sentence);
            }
        }
        sentences
    }

    /// Finds the byte index immediately after the earliest sentence boundary.
    fn find_sentence_boundary(&self) -> Option<usize> {
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        if len == 0 {
            return None;
        }

        let mut word_count = 0usize;
        let mut in_word = false;
        let mut last_clause_boundary: Option<usize> = None;

        for i in 0..len {
            let b = bytes[i];

            if b.is_ascii_whitespace() {
                if in_word {
                    word_count += 1;
                    in_word = false;
                }
            } else {
                in_word = true;
            }

            // Check for sentence terminators: '.', '!', '?'
            if b == b'.' || b == b'!' || b == b'?' {
                // Must be followed by whitespace, or newline, to be a valid boundary while streaming
                let next_is_boundary = if i + 1 < len {
                    bytes[i + 1].is_ascii_whitespace()
                } else {
                    false // Do not split on trailing punctuation during stream (wait for next chunk or flush)
                };

                if next_is_boundary {
                    // Ignore decimal numbers, e.g. "3.5" or "10.0"
                    if b == b'.' && i > 0 && i + 1 < len {
                        let prev = bytes[i - 1];
                        let next = bytes[i + 1];
                        if prev.is_ascii_digit() && next.is_ascii_digit() {
                            continue;
                        }
                    }

                    // Ignore ellipsis "..."
                    if b == b'.' {
                        if (i > 0 && bytes[i - 1] == b'.')
                            || (i + 1 < len && bytes[i + 1] == b'.')
                        {
                            continue;
                        }
                    }

                    // Ignore common abbreviations if it's a period
                    if b == b'.' {
                        let prefix = &self.buffer[..i];
                        if is_abbreviation(prefix) {
                            continue;
                        }
                    }

                    // Include trailing quotes or closing brackets if any
                    let mut end_idx = i + 1;
                    while end_idx < len
                        && (bytes[end_idx] == b'"'
                            || bytes[end_idx] == b'\''
                            || bytes[end_idx] == b')'
                            || bytes[end_idx] == b']')
                    {
                        end_idx += 1;
                    }

                    let candidate = &self.buffer[..end_idx];
                    if !candidate.chars().any(|c| c.is_alphabetic()) {
                        continue;
                    }

                    return Some(end_idx);
                }
            }

            // Clause split fallback: If an unclosed sentence has >= 8 words and hits a comma/semicolon/colon
            if (b == b',' || b == b';' || b == b':')
                && word_count >= 8
                && i + 1 < len
                && bytes[i + 1].is_ascii_whitespace()
            {
                last_clause_boundary = Some(i + 1);
            }
        }

        // If a sentence is unusually long (>= 14 words) with no period, split at the clause boundary
        if word_count >= 14 && last_clause_boundary.is_some() {
            return last_clause_boundary;
        }

        None
    }
}

fn is_abbreviation(prefix: &str) -> bool {
    let last_word = prefix
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphabetic());

    matches!(
        last_word.to_ascii_lowercase().as_str(),
        "mr" | "mrs" | "ms" | "dr" | "prof" | "sr" | "jr" | "vs" | "eg" | "ie" | "etc" | "st" | "ave"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_complete_sentences() {
        let mut chunker = SentenceChunker::new();
        let sentences = chunker.push("Hello Rahul! How are you doing? I am good.");
        assert_eq!(
            sentences,
            vec![
                "Hello Rahul!",
                "How are you doing?",
                "I am good."
            ]
        );
        assert_eq!(chunker.flush(), None);
    }

    #[test]
    fn streams_incremental_chunks() {
        let mut chunker = SentenceChunker::new();
        assert_eq!(chunker.push("Hello "), Vec::<String>::new());
        assert_eq!(chunker.push("Rahul! How "), vec!["Hello Rahul!"]);
        assert_eq!(chunker.push("are you?"), Vec::<String>::new());
        assert_eq!(chunker.flush(), Some("How are you?".to_string()));
    }

    #[test]
    fn ignores_abbreviations_and_decimals() {
        let mut chunker = SentenceChunker::new();
        let sentences = chunker.push("Dr. Smith bought 3.5 kg of apples. That was great!");
        assert_eq!(
            sentences,
            vec![
                "Dr. Smith bought 3.5 kg of apples.",
                "That was great!"
            ]
        );
    }

    #[test]
    fn flushes_unpunctuated_text() {
        let mut chunker = SentenceChunker::new();
        assert_eq!(chunker.push("Sure let me check"), Vec::<String>::new());
        assert_eq!(chunker.flush(), Some("Sure let me check".to_string()));
    }

    #[test]
    fn splits_long_clauses_for_low_latency() {
        let mut chunker = SentenceChunker::new();
        let sentences = chunker.push(
            "I checked your schedule for tomorrow morning, and you have a doctor appointment at ten AM.",
        );
        assert_eq!(
            sentences,
            vec![
                "I checked your schedule for tomorrow morning,",
                "and you have a doctor appointment at ten AM."
            ]
        );
    }

    #[test]
    fn does_not_emit_or_flush_non_alphabetic_fragments() {
        let mut chunker = SentenceChunker::new();
        assert_eq!(chunker.push("40. "), Vec::<String>::new());
        assert_eq!(chunker.flush(), None);

        let mut chunker2 = SentenceChunker::new();
        assert_eq!(chunker2.push("--- "), Vec::<String>::new());
        assert_eq!(chunker2.flush(), None);
    }
}
