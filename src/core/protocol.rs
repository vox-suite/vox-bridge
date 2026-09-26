/**
* this file code contains core communication protocol structures
*/
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct IdentityPayload<'a> {
    pub channel: &'a str,
    pub external_id: &'a str,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RespondRequest<'a> {
    pub identity: IdentityPayload<'a>,
    pub external_conversation_id: &'a str,
    pub text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initiation_context: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_provider: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filler: Option<&'a str>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RespondResponse {
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpeculateRequest<'a> {
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
    pub identity: IdentityPayload<'a>,
    pub external_conversation_id: &'a str,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostContextRequest {
    pub host_user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_external_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContextRequest {
    pub host_context: HostContextRequest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    Waiting,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    Clarification,
    Connection,
    Approval,
    Authentication,
    Reconciliation,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StartTaskRequest {
    pub title: String,
    pub instruction: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_external_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StartTaskRequestBody {
    pub host_context: HostContextRequest,
    pub task: StartTaskRequest,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WaitRequest {
    pub reason: WaitReason,
    #[serde(default)]
    pub checkpoint: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WaitRequestBody {
    pub host_context: HostContextRequest,
    pub wait: WaitRequest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DurableTask {
    pub id: Uuid,
    pub title: String,
    pub state: RunState,
    pub run_id: Uuid,
    #[serde(default)]
    pub wait_reason: Option<WaitReason>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CreateProposalRequest {
    pub span_id: Uuid,
    pub task_run_id: Uuid,
    pub agent_external_key: String,
    pub capability_external_key: String,
    pub details: serde_json::Value,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces_proposal_id: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProposeRequestBody {
    pub host_context: HostContextRequest,
    pub proposal: CreateProposalRequest,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ApproveRequestBody {
    pub host_context: HostContextRequest,
    pub details: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Proposal {
    pub id: Uuid,
    pub capability_external_key: String,
    pub expires_at: String,
    #[serde(default)]
    pub approval_id: Option<Uuid>,
    pub details: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HandoffState {
    pub target: String,
    pub reason: String,
    pub handed_off_at: String,
}
