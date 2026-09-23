/**
* this file code contains channel task, proposal, handoff, and status interaction handlers
*/
use serde_json::Value;
use uuid::Uuid;

use crate::channels::context::{CallContext, normalized_e164};
use crate::core::client::CoreClient;
use crate::core::protocol::{DurableTask, Proposal, RunState, WaitReason};
use crate::voice::provider::VoiceError;

#[derive(Debug, thiserror::Error)]
pub enum ChannelInteractionError {
    #[error("Unauthorized sender: identity '{0}' does not match active context")]
    UnauthorizedSender(String),
    #[error("Proposal not found: '{0}'")]
    ProposalNotFound(String),
    #[error("Proposal expired or superseded: '{0}'")]
    ProposalExpired(String),
    #[error("Action outcome unconfirmed: '{0}'")]
    UnconfirmedOutcome(String),
    #[error("Duplicate or replayed decision: '{0}'")]
    DuplicateDecision(String),
    #[error("Core communication failure: '{0}'")]
    CoreError(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChannelCommand {
    Status { task_id: Option<Uuid> },
    Approve { proposal_id: Uuid },
    Reject { proposal_id: Uuid },
    Clarify { task_id: Option<Uuid>, text: String },
    Text(String),
}

pub fn parse_channel_command(input: &str) -> ChannelCommand {
    let trimmed = input.trim();
    let lower = trimmed.to_lowercase();

    if lower == "status" {
        return ChannelCommand::Status { task_id: None };
    }
    if let Some(rest) = lower.strip_prefix("status ")
        && let Ok(id) = Uuid::parse_str(rest.trim())
    {
        return ChannelCommand::Status { task_id: Some(id) };
    }

    if let Some(rest) = lower.strip_prefix("approve ")
        && let Ok(id) = Uuid::parse_str(rest.trim())
    {
        return ChannelCommand::Approve { proposal_id: id };
    }
    if lower == "approve" || lower == "yes, approve" || lower == "i approve" {
        // If no ID is specified, return text unless paired with an active context
        return ChannelCommand::Text(trimmed.to_string());
    }

    if let Some(rest) = lower.strip_prefix("reject ")
        && let Ok(id) = Uuid::parse_str(rest.trim())
    {
        return ChannelCommand::Reject { proposal_id: id };
    }

    if let Some(rest) = trimmed
        .strip_prefix("clarify:")
        .or_else(|| trimmed.strip_prefix("clarification:"))
        .or_else(|| trimmed.strip_prefix("Clarify:"))
        .or_else(|| trimmed.strip_prefix("Clarification:"))
    {
        return ChannelCommand::Clarify {
            task_id: None,
            text: rest.trim().to_string(),
        };
    }

    ChannelCommand::Text(trimmed.to_string())
}

pub struct ProposalNarrator;

impl ProposalNarrator {
    pub fn narrate(proposal: &Proposal) -> String {
        let details = &proposal.details;
        let mut lines = Vec::new();

        lines.push(format!(
            "Action Proposal [{}] requires your explicit approval.",
            proposal.id
        ));
        lines.push(format!("Capability: {}", proposal.capability_external_key));

        if let Some(account) = details.get("account").or_else(|| details.get("provider"))
            && let Some(val) = account.as_str()
        {
            lines.push(format!("Provider: {val}"));
        }
        if let Some(recipient) = details
            .get("recipient")
            .or_else(|| details.get("destination"))
            && let Some(val) = recipient.as_str()
        {
            lines.push(format!("Recipient/Destination: {val}"));
        }
        if let Some(price) = details
            .get("price_minor")
            .or_else(|| details.get("fare_minor"))
            .or_else(|| details.get("price"))
        {
            let currency = details
                .get("currency")
                .and_then(Value::as_str)
                .unwrap_or("INR");
            if let Some(minor) = price.as_i64() {
                let major = minor as f64 / 100.0;
                lines.push(format!("Amount: {major:.2} {currency}"));
            } else if let Some(val) = price.as_str() {
                lines.push(format!("Amount: {val} {currency}"));
            }
        }
        if let Some(time) = details.get("time").or_else(|| details.get("at"))
            && let Some(val) = time.as_str()
        {
            lines.push(format!("Scheduled: {val}"));
        }
        if let Some(content) = details.get("message").or_else(|| details.get("summary"))
            && let Some(val) = content.as_str()
        {
            lines.push(format!("Summary: {val}"));
        }

        lines.push(format!("Expires at: {}", proposal.expires_at));
        lines.push(
            "Notice: Vox Bridge cannot execute this action independently. Your explicit approval is required before Core coordinates execution. Reply 'approve <id>' or 'reject <id>'."
                .to_string(),
        );

        lines.join("\n")
    }
}

pub struct HandoffNarrator;

impl HandoffNarrator {
    pub fn narrate(task_title: &str, task_id: Uuid, target: &str, reason: &str) -> String {
        format!(
            "Task '{task_title}' [{task_id}] has been handed off to {target}.\n\
            Reason: {reason}.\n\
            Notice: This task has been handed off for external processing and is NOT completed. \
            Bridge does not claim completion. A representative or external service will follow up."
        )
    }
}

pub struct StatusNarrator;

impl StatusNarrator {
    pub fn narrate(task: &DurableTask) -> String {
        match task.state {
            RunState::Queued => {
                format!(
                    "Task '{}' [{}] is queued and waiting to begin.",
                    task.title, task.id
                )
            }
            RunState::Running => {
                format!("Task '{}' [{}] is currently running.", task.title, task.id)
            }
            RunState::Waiting => match task.wait_reason {
                Some(WaitReason::Approval) => {
                    format!(
                        "Task '{}' [{}] is waiting for your explicit approval on a proposal.",
                        task.title, task.id
                    )
                }
                Some(WaitReason::Clarification) => {
                    format!(
                        "Task '{}' [{}] is waiting for clarification. Please provide additional details.",
                        task.title, task.id
                    )
                }
                Some(WaitReason::Connection) => {
                    format!(
                        "Task '{}' [{}] is waiting for a third-party account connection.",
                        task.title, task.id
                    )
                }
                Some(WaitReason::Authentication) => {
                    format!(
                        "Task '{}' [{}] is waiting for external authentication.",
                        task.title, task.id
                    )
                }
                Some(WaitReason::Reconciliation) => {
                    format!(
                        "Task '{}' [{}] is waiting for provider reconciliation.",
                        task.title, task.id
                    )
                }
                None => {
                    format!(
                        "Task '{}' [{}] is waiting for input or dependencies.",
                        task.title, task.id
                    )
                }
            },
            RunState::Completed => {
                format!(
                    "Task '{}' [{}] has completed successfully.",
                    task.title, task.id
                )
            }
            RunState::Cancelled => {
                format!("Task '{}' [{}] was cancelled.", task.title, task.id)
            }
            RunState::Failed => {
                format!("Task '{}' [{}] has failed.", task.title, task.id)
            }
        }
    }
}

pub struct UncertaintyNarrator;

impl UncertaintyNarrator {
    pub fn unconfirmed_outcome(action_description: &str, error_message: &str) -> String {
        format!(
            "Unconfirmed Outcome: We could not verify the outcome of {action_description} \
            due to a provider communication issue ({error_message}). \
            The authoritative run state remains unconfirmed. No action was assumed completed. \
            Please check your dashboard at app.voxagent.in or query status again shortly."
        )
    }

    pub fn partial_success(action_description: &str, details: &str) -> String {
        format!(
            "Partial Outcome: {action_description} completed partially: {details}. \
            Some steps could not be completed. The task is not finished. \
            Please review the remaining requirements on your dashboard."
        )
    }
}

pub struct ChannelInteractionHandler;

impl ChannelInteractionHandler {
    pub fn verify_sender(
        context: &CallContext,
        sender_id: &str,
    ) -> Result<(), ChannelInteractionError> {
        let normalized_sender = normalized_e164(sender_id).unwrap_or_else(|| sender_id.to_string());
        let normalized_context = normalized_e164(&context.external_identity)
            .unwrap_or_else(|| context.external_identity.clone());

        if normalized_sender != normalized_context {
            return Err(ChannelInteractionError::UnauthorizedSender(
                sender_id.to_string(),
            ));
        }
        Ok(())
    }

    pub async fn handle_approve(
        client: &CoreClient,
        context: &CallContext,
        sender_id: &str,
        proposal_id: Uuid,
        details: Value,
    ) -> Result<String, ChannelInteractionError> {
        Self::verify_sender(context, sender_id)?;

        let outcome = client
            .approve_action_proposal(context, proposal_id, details)
            .await;
        match outcome {
            Ok(proposal) => Ok(format!(
                "Proposal [{}] for capability '{}' has been approved. Core is coordinating execution.",
                proposal.id, proposal.capability_external_key
            )),
            Err(VoiceError::Provider { message, .. }) => {
                if message.contains("consumed") || message.contains("already") {
                    Err(ChannelInteractionError::DuplicateDecision(
                        "This decision has already been processed or consumed.".into(),
                    ))
                } else if message.contains("409")
                    || message.contains("expired")
                    || message.contains("superseded")
                {
                    Err(ChannelInteractionError::ProposalExpired(
                        "Proposal has expired or was superseded. Approval cannot proceed.".into(),
                    ))
                } else if message.contains("404") || message.contains("not found") {
                    Err(ChannelInteractionError::ProposalNotFound(
                        "Proposal not found or belongs to another user.".into(),
                    ))
                } else {
                    Err(ChannelInteractionError::CoreError(message))
                }
            }
            Err(e) => Err(ChannelInteractionError::UnconfirmedOutcome(e.to_string())),
        }
    }

    pub async fn handle_status(
        client: &CoreClient,
        context: &CallContext,
        sender_id: &str,
        task_id: Uuid,
    ) -> Result<String, ChannelInteractionError> {
        Self::verify_sender(context, sender_id)?;

        match client.get_durable_task(context, task_id).await {
            Ok(task) => Ok(StatusNarrator::narrate(&task)),
            Err(VoiceError::Provider { message, .. }) => {
                Err(ChannelInteractionError::CoreError(message))
            }
            Err(e) => Err(ChannelInteractionError::UnconfirmedOutcome(e.to_string())),
        }
    }
}
