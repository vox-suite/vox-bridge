use crate::channels::context::CallContext;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TurnTiming {
    pub speech_started_at: Option<Instant>,
    pub last_voiced_audio_at: Option<Instant>,
    pub transcript_received_at: Instant,
    pub last_transcript_received_at: Instant,
}

#[derive(Debug)]
pub struct ResponseMetrics {
    pub started: Instant,
    pub timing: Option<TurnTiming>,
    pub core_requested: Option<Instant>,
    pub core_opened: Option<Instant>,
    pub first_text: Option<Instant>,
    pub first_sentence: Option<Instant>,
    pub first_answer_enqueued: Option<Instant>,
    pub first_filler_enqueued: Option<Instant>,
    pub text_finished: Option<Instant>,
    pub filler_decision_ms: Option<u64>,
    pub first_tts_ttfb_ms: Option<u64>,
    pub text_chunks: u64,
    pub reply_bytes: u64,
    pub sentences: u64,
    pub tts_errors: u64,
    pub last_stage: &'static str,
}

impl ResponseMetrics {
    pub fn new(started: Instant, timing: Option<TurnTiming>) -> Self {
        Self {
            started,
            timing,
            core_requested: None,
            core_opened: None,
            first_text: None,
            first_sentence: None,
            first_answer_enqueued: None,
            first_filler_enqueued: None,
            text_finished: None,
            filler_decision_ms: None,
            first_tts_ttfb_ms: None,
            text_chunks: 0,
            reply_bytes: 0,
            sentences: 0,
            tts_errors: 0,
            last_stage: "scheduled",
        }
    }

    pub fn breakdown(&self, finished: Instant) -> LatencyBreakdown {
        let transcript = self
            .timing
            .as_ref()
            .map(|t| t.transcript_received_at)
            .unwrap_or(self.started);
        let final_transcript = self
            .timing
            .as_ref()
            .map(|t| t.last_transcript_received_at)
            .unwrap_or(transcript);
        let speech_end = self.timing.as_ref().and_then(|t| t.last_voiced_audio_at);
        LatencyBreakdown {
            speech_to_final_ms: between(speech_end, Some(final_transcript)),
            dispatch_wait_ms: between(Some(transcript), Some(self.started)),
            final_to_dispatch_ms: between(Some(final_transcript), Some(self.started)),
            filler_decision_ms: self.filler_decision_ms,
            core_headers_ms: between(self.core_requested, self.core_opened),
            core_first_text_ms: between(self.core_requested, self.first_text),
            first_sentence_ms: between(self.core_requested, self.first_sentence),
            text_to_sentence_ms: between(self.first_text, self.first_sentence),
            first_tts_ttfb_ms: self.first_tts_ttfb_ms,
            transcript_to_answer_enqueue_ms: between(Some(transcript), self.first_answer_enqueued),
            speech_end_to_answer_enqueue_ms: between(speech_end, self.first_answer_enqueued),
            transcript_to_filler_enqueue_ms: between(Some(transcript), self.first_filler_enqueued),
            response_total_ms: between(Some(self.started), Some(finished)),
            core_stream_ms: between(self.core_requested, self.text_finished),
            speech_end_source: if speech_end.is_some() {
                "earshot_audio_arrival"
            } else {
                "unavailable"
            },
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct LatencyBreakdown {
    pub speech_to_final_ms: Option<u64>,
    pub dispatch_wait_ms: Option<u64>,
    pub final_to_dispatch_ms: Option<u64>,
    pub filler_decision_ms: Option<u64>,
    pub core_headers_ms: Option<u64>,
    pub core_first_text_ms: Option<u64>,
    pub first_sentence_ms: Option<u64>,
    pub text_to_sentence_ms: Option<u64>,
    pub first_tts_ttfb_ms: Option<u64>,
    pub transcript_to_answer_enqueue_ms: Option<u64>,
    pub speech_end_to_answer_enqueue_ms: Option<u64>,
    pub transcript_to_filler_enqueue_ms: Option<u64>,
    pub response_total_ms: Option<u64>,
    pub core_stream_ms: Option<u64>,
    pub speech_end_source: &'static str,
}

fn between(start: Option<Instant>, end: Option<Instant>) -> Option<u64> {
    end?.checked_duration_since(start?)
        .map(|d| d.as_millis() as u64)
}

pub fn log_turn_latency(
    number: u64,
    context: &CallContext,
    generation: u64,
    kind: &'static str,
    metrics: &ResponseMetrics,
    outcome: &'static str,
    finished: Instant,
) {
    let m = metrics.breakdown(finished);
    tracing::info!(
        turn = number, conversation_id = %context.external_conversation_id,
        turn_id = context.turn_id.as_deref(), revision = context.revision,
        generation, kind, outcome, last_stage = metrics.last_stage,
        tts_provider = context.tts_provider.as_deref(),
        speech_end_source = m.speech_end_source,
        speech_to_final_ms = m.speech_to_final_ms,
        dispatch_wait_ms = m.dispatch_wait_ms, final_to_dispatch_ms = m.final_to_dispatch_ms,
        filler_decision_ms = m.filler_decision_ms,
        core_headers_ms = m.core_headers_ms, core_first_text_ms = m.core_first_text_ms,
        first_sentence_ms = m.first_sentence_ms, text_to_sentence_ms = m.text_to_sentence_ms,
        first_tts_ttfb_ms = m.first_tts_ttfb_ms,
        transcript_to_answer_enqueue_ms = m.transcript_to_answer_enqueue_ms,
        speech_end_to_answer_enqueue_ms = m.speech_end_to_answer_enqueue_ms,
        transcript_to_filler_enqueue_ms = m.transcript_to_filler_enqueue_ms,
        core_stream_ms = m.core_stream_ms, response_total_ms = m.response_total_ms,
        text_chunks = metrics.text_chunks, reply_bytes = metrics.reply_bytes,
        sentences = metrics.sentences, tts_errors = metrics.tts_errors,
        "VOICE_TURN_LATENCY_REPORT"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn separates_filler_answer_settling_and_core_wait() {
        let base = Instant::now();
        let at = |ms| base + Duration::from_millis(ms);
        let mut m = ResponseMetrics::new(
            at(500),
            Some(TurnTiming {
                speech_started_at: Some(base),
                last_voiced_audio_at: Some(at(100)),
                transcript_received_at: at(300),
                last_transcript_received_at: at(400),
            }),
        );
        m.core_requested = Some(at(600));
        m.core_opened = Some(at(750));
        m.first_text = Some(at(900));
        m.first_sentence = Some(at(950));
        m.first_filler_enqueued = Some(at(700));
        m.first_answer_enqueued = Some(at(1100));
        let r = m.breakdown(at(1300));
        assert_eq!(r.speech_to_final_ms, Some(300));
        assert_eq!(r.dispatch_wait_ms, Some(200));
        assert_eq!(r.final_to_dispatch_ms, Some(100));
        assert_eq!(r.core_headers_ms, Some(150));
        assert_eq!(r.core_first_text_ms, Some(300));
        assert_eq!(r.text_to_sentence_ms, Some(50));
        assert_eq!(r.transcript_to_filler_enqueue_ms, Some(400));
        assert_eq!(r.transcript_to_answer_enqueue_ms, Some(800));
        assert_eq!(r.speech_end_to_answer_enqueue_ms, Some(1000));
    }

    #[test]
    fn missing_or_out_of_order_observations_are_unknown_not_zero() {
        let base = Instant::now();
        let mut m = ResponseMetrics::new(base, None);
        let r = m.breakdown(base);
        assert_eq!(r.speech_to_final_ms, None);
        assert_eq!(r.speech_end_source, "unavailable");
        assert_eq!(r.core_first_text_ms, None);
        assert_eq!(r.transcript_to_answer_enqueue_ms, None);
        m.core_requested = Some(base + Duration::from_millis(10));
        m.first_text = Some(base);
        assert_eq!(m.breakdown(base).core_first_text_ms, None);
    }
}
