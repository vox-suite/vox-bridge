/**
* this file code contains external service provider adapters
*/
pub mod jev;
pub mod stt;
pub mod telephony;
pub mod tts;

pub use jev::JevClient;
pub use telephony::{TelephonyClient, TelephonyError};
