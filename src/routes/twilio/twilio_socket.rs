use axum::{
    extract::{
        WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::IntoResponse,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Debug)]
pub struct TwilioMediaPayload {
    pub payload: String,
    pub track: Option<String>,
    pub chunk: Option<String>,
    pub timestamp: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct TwilioStartPayload {
    #[serde(rename = "streamSid")]
    pub stream_sid: String,
    #[serde(rename = "callSid")]
    pub call_sid: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event")]
pub enum InboundStreamMessage {
    #[serde(rename = "connected")]
    Connected { protocol: String, version: String },
    #[serde(rename = "start")]
    Start { start: TwilioStartPayload },
    #[serde(rename = "media")]
    Media {
        #[serde(rename = "streamSid")]
        stream_sid: String,
        media: TwilioMediaPayload,
    },
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "mark")]
    Mark,
}

#[derive(Debug, Serialize)]
pub struct OutboundMediaMessage {
    pub event: &'static str,
    #[serde(rename = "streamSid")]
    pub stream_sid: String,
    pub media: OutboundPayload,
}

#[derive(Debug, Serialize)]
pub struct OutboundPayload {
    pub payload: String, // Base64 encoded mu-law audio
}

pub async fn voice_stream_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_voice_socket)
}

async fn handle_voice_socket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let mut current_stream_sid: Option<String> = None;

    tracing::info!("Twilio media stream connected");

    while let Some(Ok(msg)) = receiver.next().await {
        let Message::Text(raw_text) = msg else {
            continue;
        };

        let parsed: Result<InboundStreamMessage, _> = serde_json::from_str(&raw_text);

        match parsed {
            Ok(InboundStreamMessage::Connected { protocol, version }) => {
                tracing::info!(protocol, version, "Twilio handshake accepted");
            }
            Ok(InboundStreamMessage::Start { start }) => {
                tracing::info!(stream_sid = %start.stream_sid, call_sid = %start.stream_sid, "Twilio stream started");
                current_stream_sid = Some(start.stream_sid);
            }
            Ok(InboundStreamMessage::Media { stream_sid, media }) => {
                if let Ok(raw_audio_bytes) = STANDARD.decode(&media.payload) {
                    let _sample_count = raw_audio_bytes.len();
                }
            }
            Ok(InboundStreamMessage::Mark) => {}
            Ok(InboundStreamMessage::Stop) => {
                tracing::info!("Twilio sent stop signal");
                break;
            }
            Err(err) => {
                tracing::warn!(%err, "Unrecogined message frame from Twilio");
            }
        }
    }

    tracing::info!("Twilio media stream closed");
}
