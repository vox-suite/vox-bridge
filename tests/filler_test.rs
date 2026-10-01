/**
* this file code contains tests for filler acknowledgment stripping and caching
*/
use vox_bridge::voice::filler::{is_conversational_pleasantry, strip_leading_ack};

#[test]
fn strips_various_acknowledgment_prefixes() {
    assert_eq!(
        strip_leading_ack("Sure, I can help with that."),
        "I can help with that."
    );
    assert_eq!(
        strip_leading_ack("Okay, let me check your balance."),
        "let me check your balance."
    );
    assert_eq!(
        strip_leading_ack("Got it! The time is 3 PM."),
        "The time is 3 PM."
    );
    assert_eq!(
        strip_leading_ack("No problem, here is the details."),
        "here is the details."
    );
}

#[test]
fn leaves_unmatched_sentences_intact() {
    assert_eq!(
        strip_leading_ack("Your current account balance is 500."),
        "Your current account balance is 500."
    );
    assert_eq!(
        strip_leading_ack("Transferring your call now."),
        "Transferring your call now."
    );
}

#[test]
fn ignores_brackets_at_start() {
    assert_eq!(
        strip_leading_ack("[greeting] Sure, hello there."),
        "hello there."
    );
}

#[test]
fn identifies_conversational_pleasantries() {
    assert!(is_conversational_pleasantry("Cool, thanks."));
    assert!(is_conversational_pleasantry("Good, thanks."));
    assert!(is_conversational_pleasantry("Thanks!"));
    assert!(is_conversational_pleasantry("Thank you so much."));
    assert!(is_conversational_pleasantry("Sounds good, thank you."));
    assert!(is_conversational_pleasantry("That's all, thanks!"));
    assert!(is_conversational_pleasantry("Bye, have a great day!"));
    assert!(is_conversational_pleasantry("Awesome, thanks."));

    // Real queries or requests must NOT be classified as pleasantries
    assert!(!is_conversational_pleasantry(
        "What did I ask you to do in our last call?"
    ));
    assert!(!is_conversational_pleasantry("Can you check my order?"));
    assert!(!is_conversational_pleasantry(
        "Where is the nearest coffee shop?"
    ));
    assert!(!is_conversational_pleasantry(
        "I want to go on a bike ride today at 8 PM"
    ));
}
