/**
* this file code contains router setup and server initialization
*/
use axum::{
    Router,
    response::{Html, IntoResponse},
    routing::{get, post},
};
use std::sync::Arc;

use crate::auth_bridge::auth_bridge_handler;
use crate::channels::desktop::{create_desktop_session, desktop_stream_handler};
use crate::channels::notifications::{handle_notification_dispatch, handle_opt_in, handle_opt_out};
use crate::channels::outbound::handle_outbound_call;
use crate::channels::twilio::{handle_voice_status, initialize_voice_socket, voice_stream_handler};
use crate::channels::whatsapp::{wa_receive, wa_verify};
use crate::state::AppState;

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/health", get(health_handler))
        .route("/auth/bridge", get(auth_bridge_handler))
        .route("/bridge/wa", get(wa_verify).post(wa_receive))
        .route("/bridge/twilio/voice", post(initialize_voice_socket))
        .route("/bridge/twilio/voice/stream", get(voice_stream_handler))
        .route("/bridge/twilio/voice/status", post(handle_voice_status))
        .route(
            "/bridge/desktop/voice/session",
            post(create_desktop_session),
        )
        .route("/bridge/desktop/voice/stream", get(desktop_stream_handler))
        .route(
            "/internal/v1/actions/outbound-call",
            post(handle_outbound_call),
        )
        .route("/channels/outbound-call", post(handle_outbound_call))
        .route(
            "/internal/v1/notifications/dispatch",
            post(handle_notification_dispatch),
        )
        .route(
            "/channels/notifications/dispatch",
            post(handle_notification_dispatch),
        )
        .route("/internal/v1/notifications/opt-out", post(handle_opt_out))
        .route("/internal/v1/notifications/opt-in", post(handle_opt_in))
        .with_state(state)
}

pub async fn run_server(state: Arc<AppState>, port: u16) -> Result<(), std::io::Error> {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    tracing::info!(port, "Server running on http://0.0.0.0:{port}");
    axum::serve(listener, app).await
}

async fn index_handler() -> impl IntoResponse {
    Html("<h1>Hello</h1>")
}

async fn health_handler() -> &'static str {
    "ok"
}
