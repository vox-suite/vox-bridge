/**
* this file code contains tests for sentence chunking and boundary detection
*/
use vox_bridge::voice::chunker::{SentenceChunker, is_abbreviation};

#[test]
fn splits_streamed_chunks_into_sentences() {
    let mut chunker = SentenceChunker::new();
    let first = chunker.push("Hello there! How ");
    assert_eq!(first, vec!["Hello there!"]);
    let second = chunker.push("are you doing today? I am ");
    assert_eq!(second, vec!["How are you doing today?"]);
    let remaining = chunker.flush();
    assert_eq!(remaining, Some("I am".to_string()));
}

#[test]
fn handles_common_abbreviations_without_splitting() {
    let mut chunker = SentenceChunker::new();
    let res = chunker.push("Hello Dr. Smith is here. Next sentence.");
    assert_eq!(res, vec!["Hello Dr. Smith is here.", "Next sentence."]);
}

#[test]
fn handles_decimal_numbers_without_splitting() {
    let mut chunker = SentenceChunker::new();
    let res = chunker.push("The total is 3.14 dollars. Done.");
    assert_eq!(res, vec!["The total is 3.14 dollars.", "Done."]);
}

#[test]
fn identifies_abbreviations() {
    assert!(is_abbreviation("hello Mr"));
    assert!(is_abbreviation("welcome Dr"));
    assert!(is_abbreviation("call at approx"));
    assert!(!is_abbreviation("hello world"));
}

#[test]
fn breaks_long_clauses_for_low_latency() {
    let mut chunker = SentenceChunker::new();
    let res = chunker.push("Here is an exceptionally long introductory clause that has many words, and then another part.");
    assert_eq!(res.len(), 2);
    assert_eq!(
        res[0],
        "Here is an exceptionally long introductory clause that has many words,"
    );
    assert_eq!(res[1], "and then another part.");
}
