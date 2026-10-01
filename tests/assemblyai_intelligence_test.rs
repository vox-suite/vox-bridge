/**
* Integration and contract tests for AssemblyAI Word Boost and LeMUR Intelligence
*/
use vox_bridge::providers::stt::SttEvent;
use vox_bridge::providers::stt::assemblyai::{AssemblyAiStt, AudioBatcher, parse_event};

#[test]
fn test_assemblyai_word_boost_url_generation() {
    let stt = AssemblyAiStt::new("aai_test_key_123".into(), "universal-3-5-pro".into())
        .with_word_boost(vec![
            "Vox".into(),
            "Spans".into(),
            "Proposal".into(),
            "Authorize".into(),
            "MapLibre".into(),
            "Expedia".into(),
        ]);

    let url = stt.build_url().expect("URL generation must succeed");

    assert!(url.starts_with("wss://streaming.assemblyai.com/v3/ws"));
    assert!(url.contains("speech_model=universal-3-5-pro"));
    assert!(url.contains("encoding=pcm_mulaw"));
    assert!(url.contains("sample_rate=8000"));
    assert!(url.contains("word_boost="));
    // Verify encoded JSON contains target terms
    assert!(url.contains("Vox") || url.contains("%22Vox%22"));
    assert!(url.contains("Spans") || url.contains("%22Spans%22"));
}

#[test]
fn test_assemblyai_high_fidelity_audio_format() {
    let stt = AssemblyAiStt::new("aai_test_key_123".into(), "universal-3-5-pro".into())
        .with_audio_format("pcm_s16le", 16000);

    let url = stt.build_url().expect("URL generation must succeed");

    assert!(url.contains("encoding=pcm_s16le"));
    assert!(url.contains("sample_rate=16000"));
}

#[test]
fn test_assemblyai_audio_batcher_16k_pcm() {
    // 16kHz 16-bit mono = 3200 bytes per 100ms
    let mut batcher = AudioBatcher::new(3200, 1600);

    let chunk = bytes::Bytes::from(vec![127u8; 6400]); // exactly 2 batches
    let batches = batcher.push(chunk);

    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), 3200);
    assert_eq!(batches[1].len(), 3200);
    assert!(batcher.pending.is_empty());
}

#[test]
fn test_assemblyai_streaming_v3_turn_event_parsing() {
    // Test Final User Transcript
    let raw_final = r#"{
        "type": "Turn",
        "end_of_turn": true,
        "transcript": "Vox, please reserve a table at Toit for 8 PM"
    }"#;
    let event = parse_event(raw_final).expect("Must parse valid JSON");
    assert_eq!(
        event,
        Some(SttEvent::FinalTranscript(
            "Vox, please reserve a table at Toit for 8 PM".into()
        ))
    );

    // Test Partial Transcript
    let raw_partial = r#"{
        "type": "Turn",
        "end_of_turn": false,
        "transcript": "Vox please reserve"
    }"#;
    let event_partial = parse_event(raw_partial).expect("Must parse valid JSON");
    assert_eq!(
        event_partial,
        Some(SttEvent::PartialTranscript("Vox please reserve".into()))
    );

    // Test SpeechStarted event for real-time barge-in / playback interruption
    let raw_started = r#"{"type": "SpeechStarted"}"#;
    let event_started = parse_event(raw_started).expect("Must parse valid JSON");
    assert_eq!(event_started, Some(SttEvent::SpeechStarted));
}
