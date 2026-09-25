/**
* this file code contains twilio media streams wire protocol structures
*/
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Deserialize, Debug)]
pub struct TwilioMediaPayload {
    pub payload: String,
}

#[derive(Deserialize, Debug)]
pub struct TwilioStartPayload {
    #[serde(rename = "streamSid")]
    pub stream_sid: String,
    #[serde(rename = "callSid")]
    pub call_sid: String,
    #[serde(rename = "customParameters", default)]
    pub custom_parameters: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct TwilioMarkPayload {
    pub name: String,
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
    Mark {
        #[serde(rename = "streamSid")]
        stream_sid: String,
        mark: TwilioMarkPayload,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct OutboundMediaMessage<'a> {
    pub event: &'static str,
    #[serde(rename = "streamSid")]
    pub stream_sid: &'a str,
    pub media: OutboundPayload<'a>,
}

#[derive(Debug, Serialize)]
pub struct OutboundPayload<'a> {
    pub payload: &'a str,
}

#[derive(Debug, Serialize)]
pub struct OutboundMarkMessage<'a> {
    pub event: &'static str,
    #[serde(rename = "streamSid")]
    pub stream_sid: &'a str,
    pub mark: OutboundMark<'a>,
}

#[derive(Debug, Serialize)]
pub struct OutboundMark<'a> {
    pub name: &'a str,
}

#[derive(Debug, Serialize)]
pub struct OutboundClearMessage<'a> {
    pub event: &'static str,
    #[serde(rename = "streamSid")]
    pub stream_sid: &'a str,
}
