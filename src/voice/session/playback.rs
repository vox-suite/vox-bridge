/**
* this file code contains voice playback state and generation tracking
*/
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct PlaybackState {
    generation: AtomicU64,
    audio_playing: AtomicBool,
    answer_started: AtomicBool,
}

impl PlaybackState {
    pub fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            audio_playing: AtomicBool::new(false),
            answer_started: AtomicBool::new(false),
        }
    }

    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn begin(&self) -> u64 {
        self.audio_playing.store(true, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn invalidate(&self) -> u64 {
        self.audio_playing.store(false, Ordering::SeqCst);
        self.answer_started.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
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
}
