// this file code contains tests for conversational turn settling and drafting

use std::time::Duration;
use vox_bridge::voice::turn::DraftTurn;

#[test]
fn updates_partial_transcript() {
    let mut turn = DraftTurn::default();
    assert!(turn.partial("hello"));
    assert!(!turn.partial("hello"));
    assert_eq!(turn.snapshot(), "hello");
    assert!(turn.has_partial());
}

#[test]
fn appends_finished_transcripts() {
    let mut turn = DraftTurn::default();
    turn.finish("hello");
    turn.finish("world");
    assert_eq!(turn.snapshot(), "hello world");
    assert!(!turn.has_partial());
}

#[test]
fn resets_on_correction_phrases() {
    let mut turn = DraftTurn::default();
    turn.finish("send fifty dollars");
    turn.finish("no, send sixty dollars");
    assert_eq!(turn.snapshot(), "no, send sixty dollars");
}

#[test]
fn settles_faster_for_normal_text() {
    let mut turn = DraftTurn::default();
    turn.finish("hello there friend");
    assert_eq!(turn.settle_delay(), Duration::from_millis(350));
}

#[test]
fn settles_slower_for_numeric_input() {
    let mut turn = DraftTurn::default();
    turn.finish("one two three four");
    assert_eq!(turn.settle_delay(), Duration::from_millis(1200));
}
