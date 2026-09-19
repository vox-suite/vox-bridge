mod agents;
mod routes;
mod telephony;
mod voice;

use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::{Html, IntoResponse},
    routing::{get, post},
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct AppState {
    pub tx: broadcast::Sender<String>,
    pub twilio: Arc<DashMap<String, crate::routes::twilio::twilio_post::TwilioState>>,
    pub twilio_account_sid: Arc<String>,
    pub twilio_auth_token: Arc<String>,
    pub twilio_from_number: Arc<String>,
    pub service_token: Arc<String>,
    pub core_url: Arc<String>,
    pub telephony: Option<Arc<dyn crate::telephony::TelephonyClient>>,
    pub voice: Arc<crate::voice::registry::VoiceRuntime>,
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    install_crypto_provider();
    tracing_subscriber::fmt::init();

    let (tx, _rx) = broadcast::channel(100);
    let twilio_state: Arc<DashMap<String, crate::routes::twilio::twilio_post::TwilioState>> =
        Arc::new(DashMap::new());
    let voice_config = crate::voice::config::VoiceConfig::from_env()
        .expect("voice provider configuration is invalid");
    let service_token = voice_config.secrets.core_service_token.clone();
    let core_url = voice_config.secrets.core_url.clone();
    let voice = Arc::new(
        crate::voice::registry::VoiceRuntime::from_config(voice_config)
            .expect("voice provider runtime initialization failed"),
    );

    let twilio_account_sid = std::env::var("TWILIO_ACCOUNT_SID").unwrap_or_default();
    let twilio_auth_token =
        std::env::var("TWILIO_AUTH_TOKEN").expect("TWILIO_AUTH_TOKEN is missing");
    let twilio_from_number = std::env::var("TWILIO_FROM_NUMBER").unwrap_or_default();

    let telephony: Option<Arc<dyn crate::telephony::TelephonyClient>> =
        if !twilio_account_sid.is_empty() && !twilio_from_number.is_empty() {
            crate::telephony::twilio_client::TwilioApiClient::new(
                twilio_account_sid.clone(),
                twilio_auth_token.clone(),
                twilio_from_number.clone(),
                crate::routes::twilio::twilio_post::VOICE_STREAM_URL.to_string(),
                crate::routes::twilio::twilio_status::VOICE_STATUS_URL.to_string(),
            )
            .ok()
            .map(|client| Arc::new(client) as Arc<dyn crate::telephony::TelephonyClient>)
        } else {
            None
        };

    let app_state = Arc::new(AppState {
        tx,
        twilio: twilio_state,
        twilio_account_sid: Arc::new(twilio_account_sid),
        twilio_auth_token: Arc::new(twilio_auth_token),
        twilio_from_number: Arc::new(twilio_from_number),
        service_token: Arc::new(service_token),
        core_url: Arc::new(core_url.clone()),
        telephony,
        voice,
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/health", get(health_handler))
        .route("/ws", get(ws_socket_upgrade))
        .route(
            "/bridge/wa",
            get(routes::whatsapp::wa_verify).post(routes::whatsapp::wa_receive),
        )
        .route(
            "/bridge/twilio/voice",
            post(routes::twilio::twilio_post::initialize_voice_socket),
        )
        .route(
            "/bridge/twilio/voice/stream",
            get(routes::twilio::twilio_socket::voice_stream_handler),
        )
        .route(
            "/bridge/twilio/voice/status",
            post(routes::twilio::twilio_status::handle_voice_status),
        )
        .route(
            "/internal/v1/actions/outbound-call",
            post(routes::internal::outbound_call::handle_outbound_call),
        )
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();

    println!("Server running on http://0.0.0.0:3000");

    // Warm up the HTTP connection pool to vox-core asynchronously in the background
    let prewarm_url = core_url.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Ok(client) = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            let _ = client
                .get(format!("{prewarm_url}/health/ready"))
                .send()
                .await;
        }
    });

    axum::serve(listener, app).await.unwrap();
}

fn install_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("failed to install the Rustls crypto provider");
    }
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
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.tx.subscribe();

    let mut send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    let tx = state.tx.clone();

    let mut receive_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(text))) = receiver.next().await {
            let _ = tx.send(text.to_string());
        }
    });

    tokio::select! {
        _ = (&mut send_task) => receive_task.abort(),
        _ = (&mut receive_task) => send_task.abort(),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_handler_reports_ok() {
        assert_eq!(health_handler().await, "ok");
    }

    #[test]
    fn startup_installs_a_rustls_crypto_provider() {
        install_crypto_provider();

        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
