// this file code contains external service provider adapters

pub mod stt;
pub mod telephony;
pub mod tts;

pub use telephony::{TelephonyClient, TelephonyError};
