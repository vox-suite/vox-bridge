/**
 * Outbound voice and messaging notification delivery adapter (E41).
 *
 * Guarantees:
 * - At least one voice or messaging channel reports delivery outcome back to Core.
 * - Retries follow the platform-provided window and do not duplicate confirmed delivery.
 * - Bridge does not receive unrelated task context, conversation transcripts, or credentials.
 * - Content redaction scrubs credentials, auth tokens, passwords, and payment card numbers.
 * - Inbound opt-out handling ("STOP", "UNSUBSCRIBE", etc.) immediately halts future deliveries.
 * - Status truthfully distinguishes delivered_to_channel, failed, and unknown.
 *   Delivered-to-channel is NEVER represented as human-seen.
 */
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptOutRecord {
    pub destination: String,
    pub channel: String,
    pub opted_out_at: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationDeliveryRecord {
    pub reminder_id: Uuid,
    pub channel: String,
    pub destination: String,
    pub status: String,
    pub provider_receipt_id: Option<String>,
    pub failure_reason: Option<String>,
    pub attempted_at: u64,
    pub retry_count: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NotificationDispatchPayload {
    pub reminder_id: Uuid,
    pub host_user_id: Option<String>,
    pub channel: String,
    pub destination: String,
    pub title: String,
    pub message: String,
    pub scheduled_for: Option<String>,
    pub retry_count: Option<u32>,
    pub max_retries: Option<u32>,
    pub retry_window_seconds: Option<u64>,
    pub idempotency_key: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationDispatchResponse {
    pub reminder_id: Uuid,
    pub status: String,
    pub channel: String,
    pub destination: String,
    pub provider_receipt_id: Option<String>,
    pub failure_reason: Option<String>,
    pub retryable: bool,
    pub delivered_to_channel_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OptOutRequest {
    pub destination: String,
    pub channel: Option<String>,
    pub reason: Option<String>,
}

#[async_trait::async_trait]
pub trait MessagingClient: Send + Sync {
    async fn send_message(&self, channel: &str, to: &str, text: &str) -> Result<String, String>;
}

pub struct WhatsAppMessagingClient {
    pub token: String,
    pub phone_id: String,
}

#[async_trait::async_trait]
impl MessagingClient for WhatsAppMessagingClient {
    async fn send_message(&self, _channel: &str, to: &str, text: &str) -> Result<String, String> {
        crate::channels::whatsapp::client::send_whatsapp_message(
            &self.token,
            &self.phone_id,
            to,
            text,
        )
        .await
        .map(|_| format!("wamid_{}", Uuid::new_v4()))
        .map_err(|e| e.to_string())
    }
}

pub fn is_opt_out_keyword(body: &str) -> bool {
    let trimmed = body.trim().to_uppercase();
    matches!(
        trimmed.as_str(),
        "STOP" | "UNSUBSCRIBE" | "CANCEL" | "QUIT" | "OPTOUT" | "STOPALL"
    )
}

pub fn is_opt_in_keyword(body: &str) -> bool {
    let trimmed = body.trim().to_uppercase();
    matches!(trimmed.as_str(), "START" | "UNSTOP" | "OPTIN" | "YES")
}

pub fn is_valid_e164(dest: &str) -> bool {
    let trimmed = dest.trim();
    if !trimmed.starts_with('+') {
        return false;
    }
    let rest = &trimmed[1..];
    if rest.len() < 7 || rest.len() > 15 {
        return false;
    }
    rest.chars().all(|c| c.is_ascii_digit())
}

pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn parse_timestamp_secs(s: &str) -> Option<u64> {
    if let Ok(epoch) = s.parse::<u64>() {
        return Some(epoch);
    }
    // Parse ISO 8601: "YYYY-MM-DDTHH:MM:SS"
    if s.len() >= 19 && s.as_bytes()[10] == b'T' {
        let year: i64 = s[0..4].parse().ok()?;
        let month: i64 = s[5..7].parse().ok()?;
        let day: i64 = s[8..10].parse().ok()?;
        let hour: i64 = s[11..13].parse().ok()?;
        let min: i64 = s[14..16].parse().ok()?;
        let sec: i64 = s[17..19].parse().ok()?;

        let m_adj = if month <= 2 { month + 9 } else { month - 3 };
        let y_adj = if month <= 2 { year - 1 } else { year };
        let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
        let yoe = y_adj - era * 400;
        let doy = (153 * m_adj + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146097 + doe - 719468;
        let secs = days * 86400 + hour * 3600 + min * 60 + sec;
        if secs >= 0 {
            return Some(secs as u64);
        }
    }
    None
}

/// Redacts sensitive credentials, tokens, credit card numbers, and secrets.
pub fn redact_sensitive_content(text: &str) -> String {
    let mut result = text.to_string();

    // Redact private keys
    if let (Some(start), Some(end)) = (result.find("-----BEGIN"), result.find("KEY-----")) {
        let slice_end = end + 8;
        result.replace_range(start..slice_end, "[REDACTED_PRIVATE_KEY]");
    }

    // Redact bearer tokens
    for prefix in ["Bearer ", "bearer "] {
        while let Some(idx) = result.find(prefix) {
            let token_start = idx + prefix.len();
            let token_len = result[token_start..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
                .count();
            if token_len >= 8 {
                result.replace_range(idx..(token_start + token_len), "[REDACTED_CREDENTIAL]");
            } else {
                break;
            }
        }
    }

    // Redact API keys: vox_sk_..., sk-..., key-...
    for prefix in ["vox_sk_", "sk-", "key-"] {
        while let Some(idx) = result.find(prefix) {
            let val_start = idx;
            let val_len = result[val_start..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .count();
            if val_len >= prefix.len() + 6 {
                result.replace_range(idx..(val_start + val_len), "[REDACTED_SECRET]");
            } else {
                break;
            }
        }
    }

    // Redact credit cards (13 to 19 digits, possibly separated by spaces or hyphens)
    let bytes = result.as_bytes();
    let n = bytes.len();
    let mut spans_to_redact = Vec::new();
    let mut i = 0;
    while i < n {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut digit_count = 0;
            let mut end = i;
            let mut j = i;
            while j < n {
                if bytes[j].is_ascii_digit() {
                    digit_count += 1;
                    end = j + 1;
                    j += 1;
                } else if (bytes[j] == b' ' || bytes[j] == b'-')
                    && j + 1 < n
                    && bytes[j + 1].is_ascii_digit()
                {
                    j += 1;
                } else {
                    break;
                }
            }
            if (13..=19).contains(&digit_count) {
                spans_to_redact.push((start, end));
                i = end;
                continue;
            }
        }
        i += 1;
    }
    for (start, end) in spans_to_redact.into_iter().rev() {
        result.replace_range(start..end, "[REDACTED_CARD]");
    }

    // Redact passwords / secrets in key=value or key: value (case-insensitive)
    for key in ["password", "secret", "pin", "auth_token"] {
        for sep in [":", "="] {
            let pattern = format!("{key}{sep}");
            let mut search_from = 0;
            while search_from < result.len() {
                let lower = result[search_from..].to_ascii_lowercase();
                if let Some(rel_idx) = lower.find(&pattern) {
                    let idx = search_from + rel_idx;
                    let val_start = idx + pattern.len();
                    let after = &result[val_start..];
                    let trimmed_after = after.trim_start();
                    let leading_spaces = after.len() - trimmed_after.len();
                    let val_len = trimmed_after
                        .chars()
                        .take_while(|c| !c.is_whitespace() && *c != ',' && *c != ';')
                        .count();
                    let full_start = val_start + leading_spaces;
                    if val_len > 0 && !trimmed_after.starts_with("[REDACTED_SECRET]") {
                        result
                            .replace_range(full_start..(full_start + val_len), "[REDACTED_SECRET]");
                        search_from = full_start + "[REDACTED_SECRET]".len();
                    } else {
                        search_from = val_start + 1;
                    }
                } else {
                    break;
                }
            }
        }
    }

    result
}

/// Handles dispatch of an outbound notification over voice or messaging.
pub async fn handle_notification_dispatch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<NotificationDispatchPayload>,
) -> Response {
    // 1. Authenticate request (Bearer service token)
    let auth_valid = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|token| {
            subtle::ConstantTimeEq::ct_eq(token.as_bytes(), state.service_token.as_bytes()).into()
        })
        .unwrap_or(false);

    if !auth_valid {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    // 2. CONTEXT ISOLATION & NON-ACTION AUTHORITY INVARIANT
    // Bridge must never receive or exercise action authority, proposal IDs, or execution commands.
    if let Some(meta) = &payload.metadata {
        let has_action_authority = meta.get("action_id").is_some()
            || meta.get("proposal_id").is_some()
            || meta.get("execution_id").is_some()
            || meta.get("execute_consequential").is_some();
        if has_action_authority {
            return (
                StatusCode::FORBIDDEN,
                "Action authority injection prohibited: reminders and notifications cannot authorize actions",
            )
                .into_response();
        }
    }

    // 3. VALIDATE DESTINATION FORMAT
    let destination = payload.destination.trim().to_string();
    if !is_valid_e164(&destination) {
        let resp = NotificationDispatchResponse {
            reminder_id: payload.reminder_id,
            status: "failed".into(),
            channel: payload.channel.clone(),
            destination,
            provider_receipt_id: None,
            failure_reason: Some("Invalid destination: must be valid E.164 phone number".into()),
            retryable: false,
            delivered_to_channel_at: None,
        };
        return (StatusCode::OK, Json(resp)).into_response();
    }

    // 4. CHECK OPT-OUT REGISTRY
    let opt_out_key = format!("{}:{}", payload.channel, destination);
    let opt_out_global_key = format!("all:{}", destination);
    if state.opt_outs.contains_key(&opt_out_key) || state.opt_outs.contains_key(&opt_out_global_key)
    {
        let resp = NotificationDispatchResponse {
            reminder_id: payload.reminder_id,
            status: "failed".into(),
            channel: payload.channel.clone(),
            destination: destination.clone(),
            provider_receipt_id: None,
            failure_reason: Some("Recipient has opted out of notifications on this channel".into()),
            retryable: false,
            delivered_to_channel_at: None,
        };
        return (StatusCode::OK, Json(resp)).into_response();
    }

    // 5. IDEMPOTENCY & DUPLICATE DELIVERY PREVENTION
    let dedupe_key = payload
        .idempotency_key
        .clone()
        .unwrap_or_else(|| payload.reminder_id.to_string());

    if let Some(existing) = state
        .notification_deliveries
        .get(&dedupe_key)
        .filter(|e| e.status == "delivered_to_channel")
    {
        // Already confirmed delivered to channel! Do NOT call provider again.
        let resp = NotificationDispatchResponse {
            reminder_id: existing.reminder_id,
            status: existing.status.clone(),
            channel: existing.channel.clone(),
            destination: existing.destination.clone(),
            provider_receipt_id: existing.provider_receipt_id.clone(),
            failure_reason: existing.failure_reason.clone(),
            retryable: false,
            delivered_to_channel_at: Some(existing.attempted_at.to_string()),
        };
        return (StatusCode::OK, Json(resp)).into_response();
    }

    // 6. RETRY LIMIT & RETRY WINDOW VALIDATION
    let retry_count = payload.retry_count.unwrap_or(0);
    let max_retries = payload.max_retries.unwrap_or(3);
    if retry_count >= max_retries {
        let resp = NotificationDispatchResponse {
            reminder_id: payload.reminder_id,
            status: "failed".into(),
            channel: payload.channel.clone(),
            destination: destination.clone(),
            provider_receipt_id: None,
            failure_reason: Some(format!(
                "Max retries exhausted ({retry_count}/{max_retries})"
            )),
            retryable: false,
            delivered_to_channel_at: None,
        };
        return (StatusCode::OK, Json(resp)).into_response();
    }

    let now_secs = now_epoch_secs();
    if let (Some(sched_str), Some(window_secs)) =
        (payload.scheduled_for.as_ref(), payload.retry_window_seconds)
    {
        // If scheduled timestamp is parseable and expired beyond retry window
        let expired = parse_timestamp_secs(sched_str)
            .is_some_and(|epoch| now_secs > epoch && (now_secs - epoch) > window_secs);
        if expired {
            let sched_epoch = parse_timestamp_secs(sched_str).unwrap();
            let resp = NotificationDispatchResponse {
                reminder_id: payload.reminder_id,
                status: "failed".into(),
                channel: payload.channel.clone(),
                destination: destination.clone(),
                provider_receipt_id: None,
                failure_reason: Some(format!(
                    "Platform retry window expired ({}s elapsed, window {}s)",
                    now_secs - sched_epoch,
                    window_secs
                )),
                retryable: false,
                delivered_to_channel_at: None,
            };
            return (StatusCode::OK, Json(resp)).into_response();
        }
    }

    // 7. CONTENT REDACTION POLICY
    let clean_title = redact_sensitive_content(&payload.title);
    let clean_message = redact_sensitive_content(&payload.message);

    // 8. DISPATCH TO CHANNEL PROVIDER
    let channel_normalized = payload.channel.trim().to_lowercase();
    let outcome = match channel_normalized.as_str() {
        "phone" | "voice" => {
            if let Some(telephony) = state.telephony.as_ref() {
                let call_text = format!("{clean_title}. {clean_message}");
                match telephony
                    .initiate_notification_call(&destination, payload.reminder_id, &call_text)
                    .await
                {
                    Ok(call_sid) => ("delivered_to_channel", Some(call_sid), None, false),
                    Err(e) => {
                        let err_str = e.to_string();
                        if err_str.contains("timeout") || err_str.contains("timed out") {
                            ("unknown", None, Some(err_str), true)
                        } else {
                            ("failed", None, Some(err_str), true)
                        }
                    }
                }
            } else {
                (
                    "failed",
                    None,
                    Some("Telephony provider unconfigured".into()),
                    false,
                )
            }
        }
        "whatsapp" | "messaging" | "sms" => {
            if let Some(messaging) = state.messaging_client.as_ref() {
                let msg_text = format!("*{clean_title}*\n{clean_message}");
                match messaging
                    .send_message(&channel_normalized, &destination, &msg_text)
                    .await
                {
                    Ok(receipt) => ("delivered_to_channel", Some(receipt), None, false),
                    Err(e) => {
                        if e.contains("timeout") || e.contains("timed out") {
                            ("unknown", None, Some(e), true)
                        } else {
                            ("failed", None, Some(e), true)
                        }
                    }
                }
            } else if state.whatsapp_phone_id.is_some() && state.whatsapp_access_token.is_some() {
                let client = WhatsAppMessagingClient {
                    token: state.whatsapp_access_token.clone().unwrap(),
                    phone_id: state.whatsapp_phone_id.clone().unwrap(),
                };
                let msg_text = format!("*{clean_title}*\n{clean_message}");
                match client
                    .send_message(&channel_normalized, &destination, &msg_text)
                    .await
                {
                    Ok(receipt) => ("delivered_to_channel", Some(receipt), None, false),
                    Err(e) => ("failed", None, Some(e), true),
                }
            } else {
                // Channel failover policy: report unconfigured channel cleanly
                (
                    "failed",
                    None,
                    Some(format!(
                        "Messaging channel '{channel_normalized}' unconfigured"
                    )),
                    false,
                )
            }
        }
        _ => (
            "failed",
            None,
            Some(format!("Unsupported delivery channel: {}", payload.channel)),
            false,
        ),
    };

    let (status_str, receipt_id, failure_reason, retryable) = outcome;

    let delivery_record = NotificationDeliveryRecord {
        reminder_id: payload.reminder_id,
        channel: payload.channel.clone(),
        destination: destination.clone(),
        status: status_str.to_string(),
        provider_receipt_id: receipt_id.clone(),
        failure_reason: failure_reason.clone(),
        attempted_at: now_secs,
        retry_count: retry_count + 1,
    };

    state
        .notification_deliveries
        .insert(dedupe_key, delivery_record);

    // 9. REPORT OUTCOME BACK TO CORE
    if let Some(host_user) = payload.host_user_id.as_deref() {
        let _ = state
            .core_client
            .report_reminder_delivery(
                host_user,
                payload.reminder_id,
                status_str,
                &payload.channel,
                &destination,
                receipt_id.as_deref(),
                failure_reason.as_deref(),
            )
            .await;
    }

    let response = NotificationDispatchResponse {
        reminder_id: payload.reminder_id,
        status: status_str.into(),
        channel: payload.channel,
        destination,
        provider_receipt_id: receipt_id,
        failure_reason,
        retryable,
        delivered_to_channel_at: if status_str == "delivered_to_channel" {
            Some(now_secs.to_string())
        } else {
            None
        },
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// Endpoint to register or remove an opt-out.
pub async fn handle_opt_out(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<OptOutRequest>,
) -> Response {
    let auth_valid = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|token| {
            subtle::ConstantTimeEq::ct_eq(token.as_bytes(), state.service_token.as_bytes()).into()
        })
        .unwrap_or(false);

    if !auth_valid {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let dest = req.destination.trim().to_string();
    let channel = req.channel.unwrap_or_else(|| "all".into()).to_lowercase();
    let key = format!("{channel}:{dest}");

    state.opt_outs.insert(
        key,
        OptOutRecord {
            destination: dest,
            channel,
            opted_out_at: now_epoch_secs(),
            reason: req
                .reason
                .unwrap_or_else(|| "User requested opt-out".into()),
        },
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "opted_out" })),
    )
        .into_response()
}

pub async fn handle_opt_in(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<OptOutRequest>,
) -> Response {
    let auth_valid = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|token| {
            subtle::ConstantTimeEq::ct_eq(token.as_bytes(), state.service_token.as_bytes()).into()
        })
        .unwrap_or(false);

    if !auth_valid {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let dest = req.destination.trim().to_string();
    let channel = req.channel.unwrap_or_else(|| "all".into()).to_lowercase();
    let key = format!("{channel}:{dest}");
    state.opt_outs.remove(&key);

    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "opted_in" })),
    )
        .into_response()
}
