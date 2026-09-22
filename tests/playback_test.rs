// this file code contains tests for playback state generation and invalidation

use vox_bridge::voice::session::PlaybackState;

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
