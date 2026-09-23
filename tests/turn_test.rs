/**
* this file code contains tests for conversational turn settling and drafting
*/
use std::time::Duration;
use vox_bridge::voice::turn::{DraftTurn, is_backchannel};

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

#[test]
fn identifies_backchannels_accurately() {
    assert!(is_backchannel("uh-huh"));
    assert!(is_backchannel("yeah"));
    assert!(is_backchannel("okay"));
    assert!(is_backchannel("mhm"));
    assert!(is_backchannel("got it"));
    assert!(!is_backchannel("stop talking"));
    assert!(!is_backchannel("I have a question"));
}

#[test]
fn dynamic_settling_adjusts_by_completeness() {
    let mut turn = DraftTurn::default();
    turn.finish("what is the weather today");
    assert_eq!(
        turn.settle_delay_for_completeness(Some(0.92)),
        Duration::from_millis(160)
    );
    assert_eq!(
        turn.settle_delay_for_completeness(Some(0.20)),
        Duration::from_millis(1100)
    );
    assert_eq!(
        turn.settle_delay_for_completeness(None),
        Duration::from_millis(350)
    );
}
