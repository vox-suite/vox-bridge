/**
* this file code contains bridge library modules and core initialization
*/
pub mod app;
pub mod auth_bridge;
pub mod channels;
pub mod config;
pub mod core;
pub mod providers;
pub mod state;
pub mod voice;

pub mod agents {
    pub use crate::core::*;
    pub mod stt {
        pub use crate::providers::stt::assemblyai as assembly_stt;
    }
    pub mod tts {
        pub use crate::providers::tts::elevenlabs as elevenlabs_tts;
        pub use crate::providers::tts::sarvam as sarvam_tts;
    }
}

pub mod routes {
    pub use crate::channels::*;
}

pub mod telephony {
    pub use crate::channels::twilio::client as twilio_client;
    pub use crate::providers::telephony::*;
}

pub use app::{build_router, run_server};
pub use config::AppConfig;
pub use state::AppState;
