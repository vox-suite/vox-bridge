/**
* this file code contains audio processing utilities
*/
pub mod mp3;

pub mod voice_activity;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum AudioFormat {
    #[default]
    Mulaw8k,
    Pcm16k,
}

impl AudioFormat {
    pub fn bytes_per_ms(self) -> u64 {
        match self {
            Self::Mulaw8k => 8,
            Self::Pcm16k => 32,
        }
    }

    pub fn sample_bytes(self) -> u64 {
        match self {
            Self::Mulaw8k => 1,
            Self::Pcm16k => 2,
        }
    }
}
