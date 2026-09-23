/**
* this file code contains desktop wire protocol definitions
*/
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DesktopInboundText {
    Start {
        #[serde(default)]
        client_version: Option<String>,
    },
    Mark {
        name: String,
    },
    Stop,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DesktopOutboundText {
    Connected {
        session_id: String,
        sample_rate: u32,
        format: String,
    },
    Mark {
        name: String,
    },
    Clear,
}

pub fn parse_inbound_text(raw: &str) -> Result<DesktopInboundText, serde_json::Error> {
    serde_json::from_str(raw)
}

pub fn serialize_outbound_text(msg: &DesktopOutboundText) -> Result<String, serde_json::Error> {
    serde_json::to_string(msg)
}
