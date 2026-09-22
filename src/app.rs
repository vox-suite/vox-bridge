// this file code contains router setup and server initialization

use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::{Html, IntoResponse},
    routing::{get, post},
};
use std::sync::Arc;

use crate::channels::outbound::handle_outbound_call;
use crate::channels::twilio::{
    handle_voice_status, initialize_voice_socket, voice_stream_handler,
};
use crate::channels::whatsapp::{wa_receive, wa_verify};
use crate::state::AppState;

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/health", get(health_handler))
        .route("/ws", get(ws_socket_upgrade))
        .route("/bridge/wa", get(wa_verify).post(wa_receive))
        .route("/bridge/twilio/voice", post(initialize_voice_socket))
        .route("/bridge/twilio/voice/stream", get(voice_stream_handler))
        .route("/bridge/twilio/voice/status", post(handle_voice_status))
        .route(
            "/internal/v1/actions/outbound-call",
            post(handle_outbound_call),
        )
        .route("/channels/outbound-call", post(handle_outbound_call))
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

async fn ws_socket_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_wa_socket(socket, state))
}

async fn handle_wa_socket(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.tx.subscribe();
    while let Ok(msg) = rx.recv().await {
        if socket.send(Message::Text(msg.into())).await.is_err() {
            break;
        }
    }
}
