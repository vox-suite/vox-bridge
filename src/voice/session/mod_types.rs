/**
* this file code contains voice session command event and signal definitions
*/
use bytes::Bytes;

#[derive(Debug, PartialEq, Eq)]
pub enum CallEvent {
    Audio(Bytes),
    PlaybackFinished(String),
    Stop,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum CallCommand {
    Media { bytes: Bytes, generation: u64 },
    Mark { name: String, generation: u64 },
    Clear,
}

#[derive(Debug)]
pub enum SessionSignal {
    ResponseFinished { number: u64, response_text: String },
}
