// this file code contains voice session command event and signal definitions

use bytes::Bytes;

#[derive(Debug, PartialEq, Eq)]
pub enum CallEvent {
    Audio(Bytes),
    PlaybackFinished(String),
    Stop,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CallCommand {
    Media(Bytes),
    Mark(String),
    Clear,
}

#[derive(Debug)]
pub enum SessionSignal {
    ResponseFinished(u64),
}
