/**
* this file code contains voice pipeline latency metrics collection
*/
use crate::channels::context::CallContext;

#[derive(Debug, Clone)]
pub struct TurnTiming {
    pub speech_started_at: Option<std::time::Instant>,
    pub last_audio_at: Option<std::time::Instant>,
    pub transcript_received_at: std::time::Instant,
}

#[allow(clippy::too_many_arguments)]
pub fn log_turn_latency(
    number: u64,
    context: &CallContext,
    prompt: &str,
    full_response: &str,
    first_sentence: Option<&str>,
    timing: Option<TurnTiming>,
    turn_started_at: std::time::Instant,
    llm_request_start: std::time::Instant,
    first_token_at: Option<std::time::Instant>,
    first_sentence_at: Option<std::time::Instant>,
    first_tts_ttfb_ms: Option<u128>,
    first_audio_sent_at: Option<std::time::Instant>,
    all_audio_sent_at: std::time::Instant,
) {
    let transcript_rx_at = timing
        .as_ref()
        .map(|t| t.transcript_received_at)
        .unwrap_or(turn_started_at);

    let stt_speech_duration_ms = timing.as_ref().and_then(|t| {
        t.speech_started_at.map(|s| {
            t.last_audio_at
                .unwrap_or(t.transcript_received_at)
                .saturating_duration_since(s)
                .as_millis()
        })
    });

    let stt_endpointing_ms = timing.as_ref().and_then(|t| {
        t.last_audio_at.map(|a| {
            t.transcript_received_at
                .saturating_duration_since(a)
                .as_millis()
        })
    });

    let queue_wait_ms = turn_started_at.duration_since(transcript_rx_at).as_millis();

    let llm_ttft_ms = first_token_at.map(|ft| ft.duration_since(llm_request_start).as_millis());
    let llm_ttfs_ms = first_sentence_at.map(|fs| fs.duration_since(llm_request_start).as_millis());
    let tts_ttfb_ms = first_tts_ttfb_ms;

    let time_to_first_audio_ms =
        first_audio_sent_at.map(|fa| fa.duration_since(transcript_rx_at).as_millis());

    let user_perceived_delay_ms = timing.as_ref().and_then(|t| {
        t.last_audio_at
            .and_then(|a| first_audio_sent_at.map(|f| f.duration_since(a).as_millis()))
    });

    let total_audio_stream_duration_ms = first_audio_sent_at
        .map(|f| all_audio_sent_at.duration_since(f).as_millis())
        .unwrap_or(0);

    let total_turn_duration_ms = all_audio_sent_at
        .duration_since(transcript_rx_at)
        .as_millis();

    tracing::info!(
        turn = number,
        conversation_id = %context.external_conversation_id,
        turn_id = ?context.turn_id,
        prompt_len = prompt.len(),
        response_len = full_response.len(),
        first_sentence_len = first_sentence.map(str::len),
        stt_speech_duration_ms = ?stt_speech_duration_ms,
        stt_endpointing_ms = ?stt_endpointing_ms,
        queue_wait_ms = %queue_wait_ms,
        llm_ttft_ms = ?llm_ttft_ms,
        llm_ttfs_ms = ?llm_ttfs_ms,
        tts_ttfb_ms = ?tts_ttfb_ms,
        time_to_first_audio_ms = ?time_to_first_audio_ms,
        user_perceived_delay_ms = ?user_perceived_delay_ms,
        audio_stream_duration_ms = %total_audio_stream_duration_ms,
        total_turn_duration_ms = %total_turn_duration_ms,
        "VOICE_TURN_LATENCY_REPORT"
    );
}
