use axum::{
    extract::{WebSocketUpgrade, ws::WebSocket},
    response::IntoResponse,
};

pub async fn voice_stream_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_voice_socket)
}

async fn handle_voice_socket(socket: WebSocket) {}
