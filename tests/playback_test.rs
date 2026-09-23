/**
* this file code contains tests for playback state generation and invalidation
*/
use bytes::Bytes;
use vox_bridge::channels::twilio::stream::serialize_command;
use vox_bridge::voice::session::{CallCommand, PlaybackState};

#[test]
fn tracks_generations_and_invalidation() {
    let state = PlaybackState::new();
    assert_eq!(state.current_generation(), 0);
    assert!(!state.is_playing());

    let gen1 = state.begin();
    assert_eq!(gen1, 1);
    assert!(state.is_playing());
    assert!(state.accepts(1));
    assert!(!state.accepts(0));

    let gen2 = state.invalidate();
    assert_eq!(gen2, 2);
    assert!(!state.is_playing());
    assert!(!state.accepts(1));
    assert!(state.accepts(2));
}

#[test]
fn serializes_call_commands_with_generation_correctly() {
    let cmd = CallCommand::Media {
        bytes: Bytes::from_static(b"test"),
        generation: 1,
    };
    let serialized = serialize_command("test-stream", &cmd).unwrap();
    assert!(serialized.contains("\"event\":\"media\""));

    let mark = CallCommand::Mark {
        name: "response-1".to_string(),
        generation: 1,
    };
    let mark_serialized = serialize_command("test-stream", &mark).unwrap();
    assert!(mark_serialized.contains("\"event\":\"mark\""));

    let clear = CallCommand::Clear;
    let clear_serialized = serialize_command("test-stream", &clear).unwrap();
    assert!(clear_serialized.contains("\"event\":\"clear\""));
}
