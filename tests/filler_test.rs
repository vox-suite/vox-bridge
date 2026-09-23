/**
* this file code contains tests for filler acknowledgment stripping and caching
*/
use vox_bridge::voice::filler::{
    filler_for_choice, is_conversational_pleasantry, rotate_filler_for_choice, strip_leading_ack,
};

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
fn maps_jev_choices_to_prewarmed_filler_phrases() {
    assert_eq!(
        filler_for_choice("check_order"),
        "Let me check on your order."
    );
    assert_eq!(
        filler_for_choice("check_account"),
        "Let me look up your account details."
    );
    assert_eq!(
        filler_for_choice("check_inventory"),
        "Let me check the latest availability for you."
    );
    assert_eq!(
        filler_for_choice("give_me_a_moment"),
        "Give me just a moment to pull that together."
    );
    assert_eq!(
        filler_for_choice("unknown_choice"),
        "I'm looking into that."
    );
    assert_eq!(filler_for_choice("pleasantry"), "");
    assert_eq!(filler_for_choice("gratitude"), "");
}

#[test]
fn rotates_fillers_for_same_category() {
    let first = rotate_filler_for_choice("give_me_a_moment");
    let second = rotate_filler_for_choice("give_me_a_moment");
    assert!(!first.is_empty());
    assert!(!second.is_empty());
    assert_ne!(first, second);
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
