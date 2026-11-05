use std::env;

use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub vox_bridge_url: String,
    pub db: PgPool,
}

pub fn init_actix_app_state(db: PgPool) -> AppState {
    AppState {
        vox_bridge_url: env::var("VOX_BRIDGE_URL")
            .expect("VOX_BRIDGE_URL must be set in the environment"),
        db,
    }
}
