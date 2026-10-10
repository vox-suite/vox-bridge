/**
* this file code contains voice session command event and signal definitions
*/
use bytes::Bytes;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioKind {
    Answer,
    Filler,
    Apology,
}

impl AudioKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Filler => "filler",
            Self::Apology => "apology",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CallEvent {
    Audio(Bytes),
    PlaybackFinished(String),
    Stop,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum CallCommand {
    Media {
        bytes: Bytes,
        generation: u64,
        kind: AudioKind,
        queued_at: Instant,
        latency_origin_at: Option<Instant>,
    },
    Mark {
        name: String,
        generation: u64,
    },
    Clear,
}

#[derive(Debug)]
pub enum SessionSignal {
    ResponseFinished { number: u64, response_text: String },
}
