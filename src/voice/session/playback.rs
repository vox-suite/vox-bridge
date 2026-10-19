/**
* this file code contains voice playback state and generation tracking
*/
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::voice::audio::AudioFormat;
pub const PACING_LOOKAHEAD: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
struct Segment {
    text: Option<String>,
    start: u64,
    end: u64,
}

#[derive(Debug, Default)]
struct Spoken {
    generation: u64,
    reply: String,
    segments: Vec<Segment>,
    queued: u64,
    forwarded: u64,
    drains_at: Option<Instant>,
    complete: bool,
}

impl Spoken {
    fn played(&self, format: AudioFormat) -> u64 {
        let unplayed = self
            .drains_at
            .map(|at| at.saturating_duration_since(Instant::now()))
            .unwrap_or_default();
        let played = self
            .forwarded
            .saturating_sub(unplayed.as_millis() as u64 * format.bytes_per_ms());
        played - played % format.sample_bytes()
    }
}

#[derive(Debug, Default)]
pub struct PlaybackState {
    generation: AtomicU64,
    audio_playing: AtomicBool,
    answer_started: AtomicBool,
    held: AtomicBool,
    spoken: Mutex<Spoken>,
    format: AudioFormat,
}

impl PlaybackState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_format(format: AudioFormat) -> Self {
        Self {
            format,
            ..Self::default()
        }
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn begin(&self) -> u64 {
        self.audio_playing.store(true, Ordering::SeqCst);
        self.held.store(false, Ordering::SeqCst);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.spoken.lock().unwrap() = Spoken {
            generation,
            ..Spoken::default()
        };
        generation
    }

    pub fn invalidate(&self) -> u64 {
        self.audio_playing.store(false, Ordering::SeqCst);
        self.answer_started.store(false, Ordering::SeqCst);
        self.held.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn hold(&self) -> bool {
        !self.held.swap(true, Ordering::SeqCst)
    }

    pub fn resume(&self) -> bool {
        self.held.swap(false, Ordering::SeqCst)
    }

    pub fn is_held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    pub fn is_playing(&self) -> bool {
        self.audio_playing.load(Ordering::SeqCst)
    }

    pub fn set_playing(&self, playing: bool) {
        self.audio_playing.store(playing, Ordering::SeqCst);
    }

    pub fn is_answer_started(&self) -> bool {
        self.answer_started.load(Ordering::SeqCst)
    }

    pub fn set_answer_started(&self, started: bool) {
        self.answer_started.store(started, Ordering::SeqCst);
    }

    pub fn accepts(&self, generation_id: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation_id
    }

    fn with_spoken(&self, generation: u64, update: impl FnOnce(&mut Spoken)) {
        let mut spoken = self.spoken.lock().unwrap();
        if spoken.generation == generation {
            update(&mut spoken);
        }
    }

    pub fn note_reply(&self, generation: u64, chunk: &str) {
        self.with_spoken(generation, |spoken| spoken.reply.push_str(chunk));
    }

    pub fn note_sentence(&self, generation: u64, text: Option<&str>) {
        self.with_spoken(generation, |spoken| {
            spoken.segments.push(Segment {
                text: text.map(str::to_owned),
                start: spoken.queued,
                end: spoken.queued,
            })
        });
    }

    pub fn note_queued(&self, generation: u64, bytes: usize) {
        self.with_spoken(generation, |spoken| {
            spoken.queued += bytes as u64;
            if let Some(segment) = spoken.segments.last_mut() {
                segment.end = spoken.queued;
            }
        });
    }

    pub fn note_complete(&self, generation: u64) {
        self.with_spoken(generation, |spoken| spoken.complete = true);
    }

    pub fn pacing_delay(&self, generation: u64) -> Duration {
        let spoken = self.spoken.lock().unwrap();
        match spoken.drains_at {
            Some(at) if spoken.generation == generation => at
                .saturating_duration_since(Instant::now())
                .saturating_sub(PACING_LOOKAHEAD),
            _ => Duration::ZERO,
        }
    }

    pub fn note_forwarded(&self, generation: u64, bytes: usize) -> u64 {
        let mut offset = 0;
        self.with_spoken(generation, |spoken| {
            let now = Instant::now();
            let start = spoken.drains_at.filter(|at| *at > now).unwrap_or(now);
            spoken.drains_at = Some(
                start + Duration::from_micros(bytes as u64 * 1000 / self.format.bytes_per_ms()),
            );
            offset = spoken.forwarded;
            spoken.forwarded += bytes as u64;
        });
        offset
    }

    pub fn played(&self, generation: u64) -> Option<u64> {
        let spoken = self.spoken.lock().unwrap();
        (spoken.generation == generation).then(|| spoken.played(self.format))
    }

    pub fn rewind(&self, generation: u64) -> Option<u64> {
        let mut played = None;
        self.with_spoken(generation, |spoken| {
            spoken.forwarded = spoken.played(self.format);
            spoken.drains_at = None;
            played = Some(spoken.forwarded);
        });
        played
    }

    pub fn recent_speech(&self) -> String {
        let spoken = self.spoken.lock().unwrap();
        if spoken.generation != self.current_generation() {
            return String::new();
        }
        let from = spoken
            .played(self.format)
            .saturating_sub(4000 * self.format.bytes_per_ms());
        spoken
            .segments
            .iter()
            .filter(|s| s.end > from && s.start < spoken.forwarded)
            .filter_map(|s| s.text.as_deref())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Returns `(reply, heard)` when answer audio of the current generation was
    /// forwarded but the listener has not heard all of it yet.
    pub fn heard(&self) -> Option<(String, String)> {
        let spoken = self.spoken.lock().unwrap();
        if spoken.generation != self.current_generation()
            || !spoken
                .segments
                .iter()
                .any(|s| s.text.is_some() && s.start < spoken.forwarded)
        {
            return None;
        }
        let played = spoken.played(self.format);
        if spoken.complete && played >= spoken.queued {
            return None;
        }
        let mut heard = String::new();
        for segment in &spoken.segments {
            let Some(text) = segment.text.as_deref() else {
                continue;
            };
            if segment.start >= played {
                break;
            }
            let part = if segment.end <= played {
                text
            } else {
                let fraction =
                    (played - segment.start) as f64 / (segment.end - segment.start).max(1) as f64;
                let cut = (text.chars().count() as f64 * fraction) as usize;
                let prefix: String = text.chars().take(cut).collect();
                let words = prefix.rfind(char::is_whitespace).unwrap_or(0);
                &text[..words]
            };
            if !part.trim().is_empty() {
                if !heard.is_empty() {
                    heard.push(' ');
                }
                heard.push_str(part.trim());
            }
        }
        Some((spoken.reply.trim().to_owned(), heard))
    }
}
