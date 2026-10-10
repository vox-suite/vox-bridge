use super::session::AudioKind;
use std::{collections::HashMap, time::Instant};

#[derive(Default)]
pub struct TransportMetrics {
    generations: HashMap<u64, Generation>,
    marks: HashMap<String, (u64, Instant, bool)>,
}
#[derive(Default)]
struct Generation {
    frames: u64,
    bytes: usize,
    first: Option<Instant>,
    last: Option<Instant>,
    max_gap_ms: u64,
    max_queue_ms: u64,
    max_write_ms: u64,
    answer_seen: bool,
    filler_seen: bool,
    apology_seen: bool,
}
impl TransportMetrics {
    #[allow(clippy::too_many_arguments)]
    pub fn media(
        &mut self,
        generation: u64,
        kind: AudioKind,
        bytes: usize,
        queued: Instant,
        write_started: Instant,
        sent: Instant,
        latency_origin_at: Option<Instant>,
    ) {
        let stats = self.generations.entry(generation).or_default();
        let queue_ms = write_started.saturating_duration_since(queued).as_millis() as u64;
        let write_ms = sent.saturating_duration_since(write_started).as_millis() as u64;
        stats.max_queue_ms = stats.max_queue_ms.max(queue_ms);
        stats.max_write_ms = stats.max_write_ms.max(write_ms);
        if let Some(last) = stats.last {
            stats.max_gap_ms = stats
                .max_gap_ms
                .max(sent.saturating_duration_since(last).as_millis() as u64);
        }
        stats.first.get_or_insert(sent);
        stats.last = Some(sent);
        stats.frames += 1;
        stats.bytes += bytes;
        let seen = match kind {
            AudioKind::Answer => &mut stats.answer_seen,
            AudioKind::Filler => &mut stats.filler_seen,
            AudioKind::Apology => &mut stats.apology_seen,
        };
        if !*seen {
            *seen = true;
            tracing::info!(
                generation,
                audio_kind = kind.label(),
                queue_ms,
                write_ms,
                turn_to_first_ws_audio_ms = latency_origin_at
                    .and_then(|at| sent.checked_duration_since(at))
                    .map(|d| d.as_millis() as u64),
                "VOICE_TRANSPORT_FIRST_AUDIO"
            );
        }
    }
    pub fn mark(&mut self, name: String, generation: u64) {
        if self.marks.len() >= 64 {
            self.marks.clear();
        }
        let response = name.starts_with("response-");
        self.marks.insert(name, (generation, Instant::now(), false));
        self.report(generation, "mark_sent");
        if response {
            self.generations.remove(&generation);
        }
    }
    pub fn acknowledged(&mut self, name: &str) {
        if let Some((generation, sent, cleared)) = self.marks.remove(name) {
            tracing::info!(
                generation,
                mark_roundtrip_ms = sent.elapsed().as_millis() as u64,
                cleared,
                acknowledgement = if cleared {
                    "invalidated"
                } else {
                    "playback_mark"
                },
                "VOICE_TRANSPORT_MARK_ACK"
            );
        }
    }
    pub fn clear(&mut self) {
        for (_, _, cleared) in self.marks.values_mut() {
            *cleared = true;
        }
        let generations: Vec<_> = self.generations.keys().copied().collect();
        for generation in generations {
            self.report(generation, "cleared");
        }
        self.generations.clear();
    }
    fn report(&self, generation: u64, outcome: &str) {
        if let Some(s) = self.generations.get(&generation) {
            tracing::info!(
                generation,
                outcome,
                audio_frames = s.frames,
                audio_bytes = s.bytes,
                max_queue_ms = s.max_queue_ms,
                max_write_ms = s.max_write_ms,
                max_frame_gap_ms = s.max_gap_ms,
                audio_span_ms = s
                    .first
                    .zip(s.last)
                    .map(|(first, last)| last.saturating_duration_since(first).as_millis() as u64),
                "VOICE_TRANSPORT_SUMMARY"
            );
        }
    }
}
impl Drop for TransportMetrics {
    fn drop(&mut self) {
        for &generation in self.generations.keys() {
            self.report(generation, "socket_closed");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separates_answer_from_filler_and_cleared_marks() {
        let mut metrics = TransportMetrics::default();
        let at = Instant::now();
        metrics.media(1, AudioKind::Filler, 160, at, at, at, Some(at));
        assert!(!metrics.generations[&1].answer_seen);
        metrics.media(1, AudioKind::Answer, 160, at, at, at, Some(at));
        assert!(metrics.generations[&1].answer_seen);
        metrics.mark("response-1".into(), 1);
        metrics.clear();
        assert!(metrics.marks["response-1"].2);
        metrics.acknowledged("response-1");
        assert!(metrics.marks.is_empty());
    }
}
