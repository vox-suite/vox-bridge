# Vox Bridge: Channel Capability & Operator Matrix

**Document Version:** 1.0.0 (Platform V1)  
**Issue Reference:** [vox-bridge#4](https://github.com/vox-suite/vox-bridge/issues/4) (`E49`)  
**Scope:** Voice, Messaging, and Desktop Channels across the Vox Agent Platform  

---

## 1. Overview & Architecture

Vox Bridge serves as an untrusted channel adapter layer that connects human communication channels (telephony, WhatsApp messaging, and desktop audio/text) to the authoritative Vox Core engine.

### Architectural Invariants:
1. **Core Authoritative Sovereignty:** Bridge never makes independent decisions, approves actions, or assumes completed transactions. All status and action execution originates strictly in Core.
2. **Non-Action Authority in Notifications:** Reminders and outbound notifications delivered over voice or messaging strictly cannot embed action authority, payment parameters, or consequential execution triggers (Invariant 1).
3. **Truthful Channel Delivery:** `delivered_to_channel` signifies carrier/gateway acceptance only and is never worded, displayed, or reported as human-seen or confirmed read (Invariant 2).
4. **Hermetic & Cost-Protected Testing:** No test relies on paid provider behavior without an explicit `VOX_LIVE_CHANNEL_TEST=1` environment switch.
5. **Zero Credential Custody:** Bridge scrubs bearer tokens, private keys, API secrets, and credit cards from all transcripts and logs before forwarding.

---

## 2. Channel Capabilities & Operator Matrix

| Capability Area | Twilio Voice (Phone Calls) | WhatsApp (Meta Cloud / Twilio) | Desktop Voice & Text (Local) |
| :--- | :--- | :--- | :--- |
| **Authentication & Ingress** | Twilio webhook signature verification (`X-Twilio-Signature` HMAC-SHA1). Fails closed (403). | Webhook verification token & HMAC-SHA256 (`X-Hub-Signature-256`). Fails closed (403). | Bearer token authentication + single-use stream tickets with anti-replay and expiration. Fails closed (401). |
| **Identity Verification** | Normalized E.164 caller ID matched against active session context. Spoofed/mismatched senders rejected. | Sender WhatsApp ID (E.164) verified against call context. Unauthorized callers blocked. | Authenticated host user identity bound to session context. |
| **Interruption & Barge-in** | Supported: `PlaybackState::invalidate()` increments playback generation and emits `CallCommand::Clear` to flush carrier buffer immediately upon speech detection. | N/A (asynchronous messaging). | Supported: client interrupt signal cancels audio stream and clears output buffer. |
| **Turn Settling & Drafts** | Dynamic settling (350ms normal text, 1200ms numeric input). Backchannel recognition (`uh-huh`, `yeah`) ignores conversational pleasantries without interrupting agent turn. | N/A (message boundaries are discrete). | Dynamic settling for microphone streaming; immediate dispatch for typed messages. |
| **Durable Tasks & Reconnect** | Caller can request "status" or "status `<task_id>`". Bridge queries Core `/v1/durable-tasks/{id}` and narratively summarizes exact authoritative `RunState` and `WaitReason`. | User sends "status" or "status `<task_id>`". Bridge fetches authoritative state from Core and returns textual summary. | Interactive UI displays real-time task status from Core WebSocket stream. |
| **Consequential Action Proposals** | Bridge reads material proposal details (provider, recipient, exact price, currency, time, expiration) and requires explicit verbal confirmation. | Bridge sends formatted proposal message with all material details. User responds "approve `<id>`" or "reject `<id>`". | Full visual proposal card rendered in web/desktop UI with explicit approval buttons. |
| **Proposal Binding & Invalidation** | Byte-for-byte exact hash match in Core. If details changed or expired, Core returns 409 Conflict; Bridge informs caller that proposal is voided. | Core returns 409 Conflict on expired/superseded proposals; Bridge responds with `ProposalExpired` notification. | UI invalidates proposal and marks as SUPERSEDED. |
| **Unconfirmed Outcomes** | If Core or provider connectivity drops during approval, Bridge emits `UncertaintyNarrator::unconfirmed_outcome` and never assumes completion. | Bridge sends unconfirmed outcome notice with advice to inspect dashboard at `app.voxagent.in`. | Status displays "Outcome Unknown — Verification Required". |
| **Labelled Handoffs** | Handoff narrator explicitly informs caller that Vox has not completed the action and delivers verbal provider link/reference. | Handoff narrator sends provider URL with clear disclosure: "Vox has not completed this action. Please continue in [Provider]". | UI renders "Continue in [Provider] ↗" button with non-completion badge. |
| **Opt-Out & Opt-In** | Inbound verbal request or carrier opt-out halts future automated outbound calls. | Inbound "STOP", "CANCEL", "UNSUBSCRIBE" halts outbound notifications immediately. "START" restores delivery. | User-managed notification preferences in account privacy dashboard. |
| **Content Redaction** | Audio transcripts scrub credit card numbers (PANs) and bearer tokens before log storage. | Outbound and inbound messages sanitized using regex pattern matching for secrets and payment card data. | Client-side and server-side secret filtering. |
| **Paid Provider Protection** | Simulated/mock telephony unless `VOX_LIVE_CHANNEL_TEST=1` is set. | Simulated/mock messaging unless `VOX_LIVE_CHANNEL_TEST=1` is set. | Fully hermetic local processing. |

---

## 3. Operational Limitations & Boundaries

### 3.1 What Bridge DOES NOT Do (Explicit Non-Features):
1. **No In-Channel Payment Processing:** Bridge never collects, processes, or relays raw credit card CVVs, bank credentials, or payment authorization numbers over unencrypted voice or messaging channels. All payments must occur via Core-managed provider integrations or verified labelled handoffs.
2. **No Autonomous Consequential Writes:** Bridge cannot approve an action on behalf of the user. Only explicit user input matching the authenticated context and proposal ID triggers approval.
3. **No Read/Seen Confirmation Inference:** Delivery receipts confirm dispatch to the cellular carrier or WhatsApp infrastructure only. Bridge never represents a delivered message as "read" or "seen" by the human recipient.
4. **No Transaction Undo via Channel:** Cancelling or deleting a conversation in Bridge does not cancel completed third-party transactions (hotel reservations, rides, orders).

---

## 4. Failure Modes & Recovery Runbook

| Failure Scenario | Bridge Behavior | Recovery / Operator Action |
| :--- | :--- | :--- |
| **Core Unreachable (503/Timeout)** | Bridge fails closed. Emits `UnconfirmedOutcome` or `CoreError`. Refuses to invent state. | Inspect Core health at `/health`. Bridge will reconnect automatically once Core recovers. |
| **Spoofed Caller / Wrong Sender** | Rejects command with `UnauthorizedSender`. Logs security event. | Verify that caller is calling from their registered E.164 phone number. |
| **Expired / Superseded Proposal** | Core returns 409 Conflict. Bridge informs caller that proposal details changed or expired. | User must request a fresh quote or new proposal from agent. |
| **Carrier Delivery Timeout (Twilio/Meta)** | Delivery status marked as `failed` with `retryable: true`. Retried within 1-hour window. | Check Twilio / Meta API status. Ensure destination number is valid and in active service. |
| **Barge-in Voice Race Condition** | `PlaybackState::invalidate()` discards inflight audio chunks; `CallCommand::Clear` flushes buffer. | Normal behavior. No operator action required. |
| **Opt-Out Enforcement** | Dispatch rejected with `status: "failed"` and reason `"Recipient opted out"`. | Recipient must send "START" or re-enable channel in web preferences. |

---

## 5. Verification Matrix & Release Evidence

Verification is established by the following automated suites:
1. `tests/channel_acceptance_test.rs`: End-to-end multi-channel acceptance covering auth failure, interruption, reconnect, notifications, unconfirmed outcomes, and paid provider guards.
2. `tests/notifications_delivery_test.rs`: Truthful delivery, content redaction, opt-out lifecycle, and timeout/reject semantics.
3. `tests/channel_tasks_and_proposals_test.rs`: Exact approvals, changed proposal rejection, duplicate decisions, and sender verification.
4. `tests/playback_test.rs`: Audio generation invalidation and barge-in clearing.
5. `tests/turn_test.rs`: Conversational turn drafting, dynamic settling, and correction handling.
