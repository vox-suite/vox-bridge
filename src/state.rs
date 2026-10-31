use std::env;

pub struct AppState {
    pub vox_bridge_url: String,
    pub vox_session_id: String,
}

pub fn init_actix_app_state() -> AppState {
    AppState {
        vox_bridge_url: env::var("VOX_BRIDGE_URL")
            .expect("VOX_BRIDGE_URL must be set in the environment"),
        vox_session_id: env::var("VOX_SESSION_ID")
            .expect("VOX_SESSION_ID must be set in the environment"),
    }
}
