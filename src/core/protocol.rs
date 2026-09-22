// this file code contains core communication protocol structures

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct HostContext<'a> {
    pub host_user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_external_key: Option<&'a str>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IdentityPayload<'a> {
    pub channel: &'a str,
    pub external_id: &'a str,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RespondRequest<'a> {
    pub host_context: HostContext<'a>,
    pub identity: IdentityPayload<'a>,
    pub external_conversation_id: &'a str,
    pub text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiation_context: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_signature: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_provider: Option<&'a str>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RespondResponse {
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpeculateRequest<'a> {
    pub host_context: HostContext<'a>,
    pub identity: IdentityPayload<'a>,
    pub external_conversation_id: &'a str,
    pub text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_provider: Option<&'a str>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompleteRequest<'a> {
    pub host_context: HostContext<'a>,
    pub identity: IdentityPayload<'a>,
    pub external_conversation_id: &'a str,
}
