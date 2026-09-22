// this file code contains tests for filler acknowledgment stripping and caching

use vox_bridge::voice::filler::strip_leading_ack;

#[test]
fn strips_various_acknowledgment_prefixes() {
    assert_eq!(strip_leading_ack("Sure, I can help with that."), "I can help with that.");
    assert_eq!(strip_leading_ack("Okay, let me check your balance."), "let me check your balance.");
    assert_eq!(strip_leading_ack("Got it! The time is 3 PM."), "The time is 3 PM.");
    assert_eq!(strip_leading_ack("No problem, here is the details."), "here is the details.");
}

#[test]
fn leaves_unmatched_sentences_intact() {
    assert_eq!(strip_leading_ack("Your current account balance is 500."), "Your current account balance is 500.");
    assert_eq!(strip_leading_ack("Transferring your call now."), "Transferring your call now.");
}

#[test]
fn ignores_brackets_at_start() {
    assert_eq!(strip_leading_ack("[greeting] Sure, hello there."), "hello there.");
}
