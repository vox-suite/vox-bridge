/**
* this file code contains external service provider adapters
*/
pub mod jev;
pub mod lemur;
pub mod stt;
pub mod telephony;
pub mod tts;

pub use jev::JevClient;
pub use lemur::AssemblyAiLemur;
pub use telephony::{TelephonyClient, TelephonyError};
