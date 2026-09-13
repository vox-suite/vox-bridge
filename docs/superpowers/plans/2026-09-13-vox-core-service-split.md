# Vox Core Service Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create the `vox-core` repository, move agent intelligence out of Bridge, add durable events, schedules, jobs, summaries, and outbound-call actions, and run Bridge, Core API, Core Worker, and Redis through Docker Compose.

**Architecture:** Bridge remains the provider-facing voice and messaging gateway. Core is one Rust codebase with API and Worker binaries sharing domain modules and Supabase Postgres, while Redis is an optional hot cache and wake-up path. Postgres job leases and idempotency keys provide correctness without Kafka.

**Tech Stack:** Rust 2024, Axum 0.8, Tokio, SQLx/PostgreSQL, Redis, Rig/Gemini, Reqwest, Serde, UUID, Chrono, Docker Compose, Twilio, AssemblyAI, Sarvam, Exa, Google Maps.

**Spec:** `docs/superpowers/specs/2026-09-13-vox-core-service-split-design.md`

## Global Constraints

- Keep exactly two application repositories: existing `vox-bridge` and new sibling `vox-core`.
- `vox-core` produces `vox-core-api` and `vox-core-worker` from one shared library.
- Supabase Postgres is the source of truth; Redis failure must not lose or block durable work.
- Do not add Kafka, transcript-vector RAG, calendar-based summary roll-ups, or user-facing permission policy.
- Only Bridge owns provider webhooks, call streaming, STT, TTS, and outbound Twilio initiation.
- Only Core owns prompts, tools, identity, memory, events, schedules, jobs, actions, and summaries.
- Do not print credentials, signatures, raw provider response bodies, transcripts, or audio.
- Tests must replace paid network boundaries; live calls are separate verification.

---

### Task 1: Scaffold the Vox Core repository and independent binaries

**Files:**
- Create: `../vox-core/.gitignore`
- Create: `../vox-core/Cargo.toml`
- Create: `../vox-core/Cargo.lock`
- Create: `../vox-core/src/lib.rs`
- Create: `../vox-core/src/config.rs`
- Create: `../vox-core/src/http/mod.rs`
- Create: `../vox-core/src/bin/vox-core-api.rs`
- Create: `../vox-core/src/bin/vox-core-worker.rs`
- Create: `../vox-core/tests/process_boundaries.rs`

**Interfaces:**
- Produces: `Config::from_env() -> Result<Config, ConfigError>`.
- Produces: `http::router(AppState) -> axum::Router` with `/health/live` and `/health/ready`.
- Produces: independent `vox-core-api` and `vox-core-worker` binaries.

- [ ] **Step 1: Initialize the sibling repository and write the binary-boundary test**

Run `git init -b main ../vox-core`, then add a test that resolves both Cargo-provided binary paths and asserts their filenames differ:

```rust
#[test]
fn builds_independent_api_and_worker_binaries() {
    assert!(env!("CARGO_BIN_EXE_vox-core-api").ends_with("vox-core-api"));
    assert!(env!("CARGO_BIN_EXE_vox-core-worker").ends_with("vox-core-worker"));
}
```

- [ ] **Step 2: Run the boundary test and confirm it fails**

Run: `cargo test --test process_boundaries`

Expected: FAIL because the package and binaries do not exist yet.

- [ ] **Step 3: Add the minimal package, shared config, router, and binaries**

Use package name `vox-core`, edition `2024`, and add Axum, Tokio, Serde, Thiserror, Tracing, and Tracing Subscriber. Define:

```rust
#[derive(Clone)]
pub struct Config {
    pub bind_address: String,
    pub database_url: String,
    pub redis_url: Option<String>,
    pub service_token: String,
}
```

`vox-core-api` loads configuration, builds `AppState`, binds the configured address, and serves the router. `vox-core-worker` loads the same configuration and waits for shutdown through a temporary `workers::run` entry point. Both install tracing without logging configuration values.

- [ ] **Step 4: Verify both binaries and health route**

Run: `cargo test --locked && cargo build --locked --bins`

Expected: all tests pass and both binaries build.

- [ ] **Step 5: Commit Core scaffolding**

Run in `../vox-core`:

```bash
git add .gitignore Cargo.toml Cargo.lock src tests
git commit -m "feat: scaffold Vox Core runtimes"
```

### Task 2: Add the Supabase/Postgres schema and repository boundary

**Files:**
- Create: `../vox-core/migrations/20260913000000_initial_core.sql`
- Create: `../vox-core/src/db/mod.rs`
- Create: `../vox-core/src/db/jobs.rs`
- Create: `../vox-core/src/identity/mod.rs`
- Create: `../vox-core/src/conversations/mod.rs`
- Create: `../vox-core/src/events/mod.rs`
- Create: `../vox-core/src/schedules/mod.rs`
- Create: `../vox-core/src/actions/mod.rs`
- Create: `../vox-core/src/summaries/mod.rs`
- Create: `../vox-core/tests/migration_contract.rs`
- Modify: `../vox-core/src/lib.rs`
- Modify: `../vox-core/Cargo.toml`

**Interfaces:**
- Produces: typed IDs `UserId`, `ConversationId`, `EventId`, `ScheduleId`, `JobId`, and `ActionId` around UUID.
- Produces: `Db::connect(&str) -> Result<Db, sqlx::Error>` and `Db::migrate() -> Result<(), sqlx::migrate::MigrateError>`.
- Produces: the tables and unique constraints named in the approved spec.

- [ ] **Step 1: Write a migration contract test**

Add a test that reads the migration with `include_str!` and checks for all required tables plus the four duplicate-prevention constraints:

```rust
for table in ["users", "user_identities", "conversations", "messages",
    "user_profiles", "conversation_summaries", "events", "scheduled_tasks",
    "jobs", "actions", "action_attempts"] {
    assert!(MIGRATION.contains(&format!("CREATE TABLE {table}")));
}
for constraint in ["user_identities_channel_external_key",
    "events_idempotency_key_key", "schedule_occurrence_key",
    "actions_idempotency_key_key"] {
    assert!(MIGRATION.contains(constraint));
}
```

- [ ] **Step 2: Run the migration test and confirm it fails**

Run: `cargo test --test migration_contract`

Expected: FAIL because the migration is absent.

- [ ] **Step 3: Implement typed domain records and the migration**

Use UUID primary keys, `TIMESTAMPTZ` timestamps, typed lifecycle text columns with `CHECK` constraints, and JSONB only for event payloads, structured agent output, and sanitized provider metadata. `jobs` must contain `state`, `attempt_count`, `next_attempt_at`, `lease_owner`, and `lease_expires_at`. `scheduled_tasks` must contain `next_run_at`, optional recurrence text, IANA timezone, and active/paused state.

- [ ] **Step 4: Add the SQLx pool and migration runner**

Configure a bounded `PgPool` with an acquisition timeout. Expose the pool only through `Db`; feature modules receive `Db` clones rather than reading `DATABASE_URL`.

- [ ] **Step 5: Verify schema contracts and compile-time types**

Run: `cargo test --locked && cargo clippy --locked --all-targets -- -D warnings`

Expected: all tests and strict Clippy pass without a live database.

- [ ] **Step 6: Commit the persistence foundation**

```bash
git add Cargo.toml Cargo.lock migrations src tests/migration_contract.rs
git commit -m "feat: add durable Core data model"
```

### Task 3: Move Gemini and its tools into Core

**Files:**
- Create: `../vox-core/src/agents/mod.rs`
- Create: `../vox-core/src/agents/conversation.rs`
- Create: `../vox-core/src/agents/event_planner.rs`
- Create: `../vox-core/src/agents/summarizer.rs`
- Create: `../vox-core/src/agents/tools/mod.rs`
- Create: `../vox-core/src/agents/tools/web_search.rs`
- Create: `../vox-core/src/agents/tools/google_maps.rs`
- Create: `../vox-core/src/agents/tools/dependencies.rs`
- Create: `../vox-core/tests/agent_contracts.rs`
- Modify: `../vox-core/src/config.rs`
- Modify: `../vox-core/src/lib.rs`
- Modify: `../vox-core/Cargo.toml`

**Interfaces:**
- Consumes: Core `Config` and shared Reqwest client.
- Produces: `ConversationAgent::respond(ConversationPrompt) -> Result<String, AgentError>`.
- Produces: `EventPlanner::plan(EventPlanningPrompt) -> Result<Vec<PlannedAction>, AgentError>`.
- Produces: `Summarizer::summarize(SummaryPrompt) -> Result<StructuredSummary, AgentError>`.
- Produces: `PlannedAction::OutboundCall { reason: String, opening_instruction: String }`.
- Produces: `ConversationPrompt { user_context: UserContext, recent_messages: Vec<PromptMessage>, user_text: String, initiation_context: Option<String> }`.
- Produces: `EventPlanningPrompt { user_context: UserContext, event_type: String, occurred_at: DateTime<Utc>, payload: serde_json::Value }`.
- Produces: `SummaryPrompt { messages: Vec<PromptMessage> }` and `PromptMessage { role: MessageRole, text: String }`.

- [ ] **Step 1: Write typed agent-contract tests**

Test that valid planner JSON becomes `PlannedAction::OutboundCall`, unknown action kinds fail, summaries reject missing required fields, and agent errors never contain a supplied provider response body.

```rust
let action = parse_planned_actions(
    r#"{"version":1,"actions":[{"kind":"outbound_call","reason":"Weather warning","opening_instruction":"Explain the warning"}]}"#,
)?;
assert!(matches!(action.as_slice(), [PlannedAction::OutboundCall { .. }]));
```

- [ ] **Step 2: Run the contract test and confirm it fails**

Run: `cargo test --test agent_contracts`

Expected: FAIL because the agent modules do not exist.

- [ ] **Step 3: Move provider-neutral tool behavior and define typed outputs**

Port Exa and Google Maps request construction from Bridge without copying logs that expose query contents. Reuse a single Reqwest client. Preserve the ten-turn Gemini maximum. Add strict Serde structures with `deny_unknown_fields` for workflow-affecting outputs.

- [ ] **Step 4: Implement the three agent responsibilities**

Use one Gemini client factory but distinct prompts. Conversation responses remain text. Planner and summarizer requests require JSON matching the versioned typed contracts; strip optional Markdown code fences before one parse attempt and reject all other malformed output.

- [ ] **Step 5: Verify tools and contracts without paid calls**

Run: `cargo test --locked && cargo clippy --locked --all-targets -- -D warnings`

Expected: request/parse tests pass; the live Gemini test remains explicitly ignored.

- [ ] **Step 6: Commit the agent move on Core**

```bash
git add Cargo.toml Cargo.lock src/agents src/config.rs src/lib.rs tests/agent_contracts.rs
git commit -m "feat: add Core agents and tools"
```

### Task 4: Implement identity, memory, conversation persistence, and the Core response API

**Files:**
- Create: `../vox-core/src/memory/mod.rs`
- Create: `../vox-core/src/cache/mod.rs`
- Create: `../vox-core/src/http/auth.rs`
- Create: `../vox-core/src/http/conversations.rs`
- Create: `../vox-core/tests/conversation_api.rs`
- Modify: `../vox-core/src/http/mod.rs`
- Modify: `../vox-core/src/identity/mod.rs`
- Modify: `../vox-core/src/conversations/mod.rs`
- Modify: `../vox-core/src/bin/vox-core-api.rs`
- Modify: `../vox-core/Cargo.toml`

**Interfaces:**
- Produces: `ChannelIdentity { channel: String, external_id: String }`.
- Produces: `RespondRequest { identity: ChannelIdentity, external_conversation_id: String, text: String, initiation_context: Option<String> }`.
- Produces: `RespondResponse { conversation_id: ConversationId, text: String }`.
- Produces: `MemoryStore::load(UserId) -> Result<UserContext, MemoryError>` with Postgres fallback.
- Produces: `UserContext { profile: BTreeMap<String, String>, active_commitments: Vec<String>, recent_summaries: Vec<String>, milestones: Vec<String> }`.

- [ ] **Step 1: Write API tests with fake repositories and agent**

Cover missing/invalid bearer token, stable identity resolution, ordered message persistence, Redis miss fallback, and sanitized agent failure. The success assertion is:

```rust
assert_eq!(response.status(), StatusCode::OK);
let body: RespondResponse = decode(response).await;
assert_eq!(body.text, "Hello Rahul");
assert_eq!(fake_messages.roles().await, ["user", "assistant"]);
```

- [ ] **Step 2: Run the focused API tests and confirm they fail**

Run: `cargo test --test conversation_api`

Expected: FAIL because the route and application service are missing.

- [ ] **Step 3: Implement bearer authentication and identity resolution**

Compare the `Authorization: Bearer` value using a constant-time equality helper. Resolve `(channel, external_id)` in one transaction, inserting a user and mapping only when no mapping exists. Never log `external_id`.

- [ ] **Step 4: Implement bounded memory with optional Redis**

Use a versioned Redis key `vox:user:{user_id}:context:v1`. On missing key, malformed cache value, connection error, or timeout, read the bounded projection from Postgres and attempt a best-effort cache refresh. The response path must succeed when Redis is absent.

- [ ] **Step 5: Implement `POST /v1/conversations/respond`**

Resolve identity and conversation, load context, store the user message, invoke `ConversationAgent`, store the assistant message, and return JSON. Use the external conversation ID plus channel as the stable provider mapping. Do not hold a database transaction during Gemini or tool calls.

- [ ] **Step 6: Verify the Core response boundary**

Run: `cargo test --locked && cargo clippy --locked --all-targets -- -D warnings`

Expected: all API tests pass with fake network boundaries.

- [ ] **Step 7: Commit the Core conversation path**

```bash
git add Cargo.toml Cargo.lock src tests/conversation_api.rs
git commit -m "feat: add persistent Core conversations"
```

### Task 5: Replace the Bridge-local agent with the Core client

**Files:**
- Create: `src/agents/core_client.rs`
- Create: `src/voice/context.rs`
- Modify: `src/agents/mod.rs`
- Modify: `src/voice/mod.rs`
- Modify: `src/voice/provider.rs`
- Modify: `src/voice/registry.rs`
- Modify: `src/voice/session.rs`
- Modify: `src/routes/twilio/twilio_socket.rs`
- Modify: `src/routes/twilio/twilio_post.rs`
- Modify: `src/main.rs`
- Modify: `Cargo.toml`
- Modify: `README.md`
- Delete: `src/agents/agent.rs`
- Delete: `src/agents/tools/web_search.rs`
- Delete: `src/agents/tools/google_maps.rs`
- Delete: `src/agents/tools/tool_dependencies.rs`
- Delete: `src/agents/tools/mod.rs`

**Interfaces:**
- Consumes: Core `POST /v1/conversations/respond`.
- Produces: `CallContext { channel, external_identity, external_conversation_id, initiation_context }`.
- Changes: `AgentProvider::respond(&self, context: &CallContext, transcript: &str)`.
- Produces: `CoreAgentClient` implementing `AgentProvider`.

- [ ] **Step 1: Write failing Core-client and call-context tests**

Use a local Axum test server to assert the bearer header and exact JSON request. Extend voice-session fakes to assert that `CallContext` reaches each response. Add a route test proving the accepted Twilio caller number becomes the external identity without logging it.

- [ ] **Step 2: Run the focused Bridge tests and confirm they fail**

Run: `cargo test --locked core_client voice_session twilio_socket`

Expected: FAIL because `CoreAgentClient` and `CallContext` do not exist.

- [ ] **Step 3: Add Core configuration and client**

Read `VOX_CORE_URL` and `VOX_CORE_SERVICE_TOKEN` at startup. Build one Reqwest client with a bounded response timeout. Map any non-success or decode error to a sanitized `VoiceError::Provider { provider: "vox-core", ... }` without including the response body.

- [ ] **Step 4: Thread call context through the voice pipeline**

Retrieve the accepted `TwilioState` for the Call SID, normalize `from` as the phone identity, create `CallContext`, and pass it into `run_voice_session`. Update every fake provider and session test to use the new method signature.

- [ ] **Step 5: Remove Bridge agent/tool ownership**

Switch the registry from `GeminiAgent` to `CoreAgentClient`, remove Gemini/Exa/Google Maps configuration requirements from Bridge, delete the moved modules, and document that those credentials now belong to Core.

- [ ] **Step 6: Verify the complete mocked inbound voice path**

Run: `cargo fmt --check && cargo test --locked && cargo clippy --locked --all-targets -- -D warnings && cargo build --locked --release`

Expected: all existing voice tests and new Core-client tests pass.

- [ ] **Step 7: Commit the Bridge wiring**

```bash
git add Cargo.toml Cargo.lock README.md src
git commit -m "feat: route voice agent turns through Core"
```

### Task 6: Implement durable job claiming and Worker recovery

**Files:**
- Create: `../vox-core/src/jobs/mod.rs`
- Create: `../vox-core/src/jobs/runner.rs`
- Create: `../vox-core/src/workers/mod.rs`
- Create: `../vox-core/tests/job_claiming.rs`
- Modify: `../vox-core/src/db/jobs.rs`
- Modify: `../vox-core/src/bin/vox-core-worker.rs`
- Modify: `../vox-core/src/lib.rs`

**Interfaces:**
- Produces: `JobKind::{ProcessEvent, RunSchedule, DispatchAction, SummarizeConversation}`.
- Produces: `JobRepository::claim(worker_id, now, lease_duration, limit) -> Vec<ClaimedJob>`.
- Produces: `JobRepository::{complete,retry,fail}` requiring matching lease owner.
- Produces: `Worker::run(CancellationToken) -> Result<(), WorkerError>`.

- [ ] **Step 1: Write database-backed job lease tests**

Against `TEST_DATABASE_URL`, insert one pending job, race two claimers, and assert only one receives it. Advance the injected clock past `lease_expires_at` and assert another worker can reclaim it. Mark complete and assert it cannot be reclaimed.

- [ ] **Step 2: Run the lease tests and confirm they fail**

Run: `cargo test --test job_claiming -- --test-threads=1`

Expected: FAIL because claiming is not implemented; skip with an explicit message only when `TEST_DATABASE_URL` is absent.

- [ ] **Step 3: Implement transactional claiming**

Use one SQL statement built around `FOR UPDATE SKIP LOCKED`, ordered by `next_attempt_at`, updating selected jobs to `running` with worker and lease fields and returning them. Completion and retry updates must include `WHERE lease_owner = $worker_id AND state = 'running'`.

- [ ] **Step 4: Implement the Worker loop and handler registry**

Poll immediately and then at a one-second interval. Dispatch each claimed job to a typed handler with bounded concurrency. Retry transient errors with capped exponential backoff; move validation and exhausted-attempt failures to terminal `failed`. Redis publish/listen may wake the loop but polling remains authoritative.

- [ ] **Step 5: Verify job recovery and graceful shutdown**

Run: `cargo test --locked && cargo clippy --locked --all-targets -- -D warnings`

Expected: job lease tests pass when the test database is provided and all offline tests always pass.

- [ ] **Step 6: Commit the Worker foundation**

```bash
git add src tests/job_claiming.rs
git commit -m "feat: add durable Core job worker"
```

### Task 7: Add idempotent event ingestion and planner processing

**Files:**
- Create: `../vox-core/src/http/events.rs`
- Create: `../vox-core/src/events/service.rs`
- Create: `../vox-core/src/events/handler.rs`
- Create: `../vox-core/tests/event_flow.rs`
- Modify: `../vox-core/src/http/mod.rs`
- Modify: `../vox-core/src/events/mod.rs`
- Modify: `../vox-core/src/workers/mod.rs`

**Interfaces:**
- Produces: `POST /v1/events -> 202 Accepted`.
- Produces: `IngestEventRequest { idempotency_key, identity, event_type, occurred_at, payload }`.
- Consumes: `EventPlanner::plan`.
- Produces: stored actions with idempotency key `event:{event_id}:action:{index}`.

- [ ] **Step 1: Write duplicate-ingestion and planning tests**

Post the same request twice and assert both responses name the same event ID while the fake repository contains one event and one job. Feed a claimed event to a fake planner returning one outbound call and assert one action is persisted after repeated handling.

- [ ] **Step 2: Run event-flow tests and confirm they fail**

Run: `cargo test --test event_flow`

Expected: FAIL because the endpoint and handler are absent.

- [ ] **Step 3: Implement atomic event ingestion**

Validate non-empty event type/idempotency key and an occurrence timestamp. Resolve identity, insert the event, and insert its `ProcessEvent` job in one transaction. On the unique idempotency constraint, load and return the existing event.

- [ ] **Step 4: Implement event planning**

Load the durable event and bounded user context, call `EventPlanner`, validate every typed action, and insert actions transactionally using deterministic action idempotency keys. Enqueue one `DispatchAction` job per newly inserted action.

- [ ] **Step 5: Verify idempotency and typed planning**

Run: `cargo test --locked && cargo clippy --locked --all-targets -- -D warnings`

Expected: duplicate requests and job retries create no duplicate actions.

- [ ] **Step 6: Commit event processing**

```bash
git add src tests/event_flow.rs
git commit -m "feat: process Core events into actions"
```

### Task 8: Add durable one-time and recurring schedules

**Files:**
- Create: `../vox-core/src/http/schedules.rs`
- Create: `../vox-core/src/schedules/service.rs`
- Create: `../vox-core/src/schedules/ticker.rs`
- Create: `../vox-core/tests/schedule_flow.rs`
- Modify: `../vox-core/src/http/mod.rs`
- Modify: `../vox-core/src/schedules/mod.rs`
- Modify: `../vox-core/src/workers/mod.rs`
- Modify: `../vox-core/Cargo.toml`

**Interfaces:**
- Produces: `POST /v1/schedules` and `PATCH /v1/schedules/{id}`.
- Produces: `ScheduleKind::{Once, Recurring { expression, timezone }}`.
- Produces: one unique `RunSchedule` job per `(schedule_id, occurrence_at)`.

- [ ] **Step 1: Write schedule lifecycle tests with an injected clock**

Test one-time creation, pause/resume, due materialization, duplicate ticker passes, and a recurring schedule in `Asia/Kolkata`. Assert two ticker passes at the same instant create one occurrence job and that the recurring schedule advances beyond that instant.

- [ ] **Step 2: Run schedule tests and confirm they fail**

Run: `cargo test --test schedule_flow`

Expected: FAIL because schedule services are absent.

- [ ] **Step 3: Implement schedule validation and management routes**

Require a future `run_at` for one-time schedules. For recurrence, parse the expression, validate the IANA timezone, and compute the first UTC occurrence. PATCH accepts exactly one operation—pause, resume, or replace timing—and returns the updated schedule.

- [ ] **Step 4: Implement atomic due-schedule materialization**

Claim due active schedules with `FOR UPDATE SKIP LOCKED`. Insert the occurrence and `RunSchedule` job with a uniqueness constraint, then mark one-time schedules complete or compute recurring `next_run_at`, all in the same transaction.

- [ ] **Step 5: Convert occurrences into normal actions**

The `RunSchedule` handler loads the schedule instruction, creates a deterministic event-like planning prompt, and uses the same typed planner and `DispatchAction` pipeline as external events.

- [ ] **Step 6: Verify schedule behavior**

Run: `cargo test --locked && cargo clippy --locked --all-targets -- -D warnings`

Expected: time-zone, recurrence, and duplicate occurrence tests pass.

- [ ] **Step 7: Commit scheduling**

```bash
git add Cargo.toml Cargo.lock src tests/schedule_flow.rs
git commit -m "feat: add durable Core schedules"
```

### Task 9: Add post-conversation summaries and Redis projections

**Files:**
- Create: `../vox-core/src/summaries/handler.rs`
- Create: `../vox-core/src/memory/projection.rs`
- Create: `../vox-core/src/http/conversation_events.rs`
- Create: `../vox-core/tests/summary_flow.rs`
- Modify: `../vox-core/src/summaries/mod.rs`
- Modify: `../vox-core/src/memory/mod.rs`
- Modify: `../vox-core/src/cache/mod.rs`
- Modify: `../vox-core/src/http/mod.rs`
- Modify: `../vox-core/src/workers/mod.rs`

**Interfaces:**
- Produces: `POST /v1/conversations/complete`.
- Produces: `CompleteConversationRequest { identity: ChannelIdentity, external_conversation_id: String }`.
- Produces: `StructuredSummary { recap, profile_updates, commitments, decisions }`.
- Produces: `UserContextProjection` with a fixed serialized byte limit.

- [ ] **Step 1: Write summary durability and cache-failure tests**

Mark a conversation complete twice and assert one summary job. Run the handler with a fake summarizer and failing cache; assert the summary and profile merge commit successfully. Assert projection construction drops oldest inactive summaries before active commitments or profile facts.

- [ ] **Step 2: Run summary tests and confirm they fail**

Run: `cargo test --test summary_flow`

Expected: FAIL because completion and projection handling are absent.

- [ ] **Step 3: Implement idempotent completion and summarization**

Completion marks the conversation once and enqueues `SummarizeConversation`. The handler loads ordered messages, invokes `Summarizer`, inserts one immutable summary, and merges versioned profile fields in a database transaction.

- [ ] **Step 4: Build and refresh the bounded context projection**

Construct profile facts, active commitments, the newest summaries, and older milestone recaps in priority order. Enforce a fixed 16 KiB serialized JSON limit by removing oldest inactive episodic entries. Write Redis only after Postgres commits; cache failure logs only user and job UUIDs.

- [ ] **Step 5: Notify Core when Bridge calls end**

Bridge sends the completion request after the Twilio media stream closes. Failure is logged and does not delay socket cleanup; Core's idempotent endpoint permits retry from future provider status callbacks.

- [ ] **Step 6: Verify summary and fallback behavior in both repositories**

Run Core and Bridge full tests and strict Clippy.

Expected: repeated completion is idempotent and Redis failure preserves the durable summary.

- [ ] **Step 7: Commit both repository changes separately**

Core commit: `feat: summarize completed conversations`

Bridge commit: `feat: report completed calls to Core`

### Task 10: Implement outbound-call action delivery

**Files:**
- Create: `../vox-core/src/bridge_client/mod.rs`
- Create: `../vox-core/src/actions/handler.rs`
- Create: `../vox-core/tests/action_dispatch.rs`
- Create: `src/routes/internal/mod.rs`
- Create: `src/routes/internal/outbound_call.rs`
- Create: `src/telephony/mod.rs`
- Create: `src/telephony/twilio_client.rs`
- Modify: `../vox-core/src/actions/mod.rs`
- Modify: `../vox-core/src/workers/mod.rs`
- Modify: `src/routes/mod.rs`
- Modify: `src/routes/twilio/twilio_post.rs`
- Modify: `src/routes/twilio/twilio_socket.rs`
- Modify: `src/routes/twilio/mod.rs`
- Modify: `src/main.rs`
- Modify: both `Cargo.toml` files

**Interfaces:**
- Core consumes: `POST /internal/v1/actions/outbound-call` on Bridge.
- Bridge consumes: Twilio Calls API and Core conversation response API.
- Produces: idempotent `OutboundCallRequest { action_id, identity, reason, opening_instruction, conversation_id }`.
- Produces: `OutboundCallResponse { provider_call_id }`.
- Produces: Core `POST /v1/actions/{id}/result` accepting `ActionResultRequest { status: ActionResultStatus, provider_call_id: String, error_code: Option<String> }`.
- Produces: signed Bridge `POST /bridge/twilio/voice/status` for Twilio status callbacks.

- [ ] **Step 1: Write Core dispatch tests**

Use a fake Bridge server. Assert a pending outbound action sends one authenticated request, records one attempt, and completes with the returned provider call ID. Retry the same job and assert no second request after completion.

- [ ] **Step 2: Write Bridge outbound-call tests**

Use a fake Twilio server. Assert missing/invalid Core bearer tokens fail, repeated `action_id` returns the same Call SID, the request contains the expected destination and inline stream TwiML, and the accepted call state includes the opening instruction.

- [ ] **Step 3: Run focused tests and confirm they fail**

Run Core `cargo test --test action_dispatch` and Bridge `cargo test outbound_call`.

Expected: FAIL because neither delivery adapter exists.

- [ ] **Step 4: Implement the Core action handler**

Claim the action for an attempt, send the action ID as the Bridge idempotency key, and record sanitized success/failure. Successful initiation marks the action `in_progress` and stores the provider call ID; it does not mark the call completed. Transient initiation errors retry through the job runner; validation errors fail terminally.

- [ ] **Step 5: Implement authenticated and idempotent Bridge delivery**

Validate the Core service bearer token. Store action-to-provider-call mapping before returning success. Call Twilio with Basic authentication from `TWILIO_ACCOUNT_SID` and `TWILIO_AUTH_TOKEN`, `TWILIO_FROM_NUMBER`, destination number, inline TwiML connecting to the existing stream route with non-secret custom parameters, and a status callback targeting Bridge.

- [ ] **Step 6: Play the outbound opening response**

When an outbound stream starts, create `CallContext` from stored call state and request a Core response using `initiation_context` before processing inbound speech. Use the existing TTS and Twilio media path, preserving barge-in behavior.

- [ ] **Step 7: Record the final provider result**

Validate Twilio's signature on the status callback, resolve its stored Core action ID, and send a sanitized terminal result to Core. Core accepts only `succeeded` or `failed`, verifies the provider call ID matches the initiated action, and applies repeated identical callbacks idempotently.

- [ ] **Step 8: Verify mocked end-to-end delivery**

Run both full test suites, strict Clippy, and release builds.

Expected: one planned action creates exactly one fake Twilio call, one opening response, and one terminal Core action result.

- [ ] **Step 9: Commit each repository**

Core commit: `feat: dispatch outbound call actions`

Bridge commit: `feat: initiate Core outbound calls`

### Task 11: Add production-oriented Docker builds and Compose wiring

**Files:**
- Create: `../vox-core/Dockerfile`
- Create: `../vox-core/compose.yaml`
- Create: `../vox-core/.dockerignore`
- Create: `../vox-core/.env.example`
- Create: `../vox-core/README.md`
- Create: `Dockerfile`
- Create: `.dockerignore`
- Create: `.env.example`
- Modify: `README.md`

**Interfaces:**
- Produces: images `vox-core` and `vox-bridge` running as non-root users.
- Produces: Compose services `bridge`, `core-api`, `core-worker`, and `redis`.
- Exposes: Bridge port `3000`; Core API and Redis remain internal.

- [ ] **Step 1: Write container contract checks**

Create a small shell-based test invoked by CI that runs `docker compose config` and asserts the rendered service list, that only Bridge has `ports`, that Core API and Worker share the Core image, and that Redis has `appendonly yes` plus a named volume.

- [ ] **Step 2: Run the Compose contract and confirm it fails**

Run: `docker compose -f ../vox-core/compose.yaml config`

Expected: FAIL because the Compose file is absent.

- [ ] **Step 3: Add multi-stage non-root Dockerfiles**

Build release binaries using locked dependencies. Copy only the runtime binary and required CA certificates into the final image. Create and switch to an unprivileged user. Add an HTTP health check for Bridge and Core API; Worker uses a process health check that does not depend on Core API.

- [ ] **Step 4: Add the four-service composition**

Build Bridge from `../vox-bridge` and Core from `.`. Set `VOX_CORE_URL=http://core-api:3001`, `REDIS_URL=redis://redis:6379`, and pass `DATABASE_URL` from the runtime environment. Use one private network, a persistent Redis volume, restart policies, and `stop_grace_period`. Do not publish Core API or Redis ports.

- [ ] **Step 5: Add safe environment examples and runbooks**

Document which credentials belong to Bridge versus Core, how to run migrations, how to start the stack, how to inspect health, and how to use an externally hosted Redis by replacing `REDIS_URL` and disabling the Compose Redis service.

- [ ] **Step 6: Build and validate the stack**

Run:

```bash
docker compose -f ../vox-core/compose.yaml config
docker compose -f ../vox-core/compose.yaml build
```

Expected: configuration renders without secret values and both images build.

- [ ] **Step 7: Commit Docker setup in both repositories**

Core commit: `build: add Vox service composition`

Bridge commit: `build: containerize Vox Bridge`

### Task 12: Run full verification and document external gaps

**Files:**
- Modify: `../vox-core/README.md`
- Modify: `README.md`
- Modify: this plan to check completed steps

**Interfaces:**
- Consumes: every previous task.
- Produces: verified local build and explicit list of checks requiring Supabase, server Redis, or paid Twilio credentials.

- [ ] **Step 1: Run Core verification**

Run:

```bash
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release --bins
```

Expected: all commands pass in `../vox-core`.

- [ ] **Step 2: Run Bridge verification**

Run the same formatting, test, strict Clippy, and release-build commands in `vox-bridge`.

Expected: all commands pass and no Bridge module imports Rig, Gemini, Exa, or Google Maps.

- [ ] **Step 3: Run Compose and mocked integration verification**

Render Compose, build both images, start the stack with test provider endpoints and a test Postgres database, wait for health, submit one event twice, and assert one planned action, one outbound Bridge request, and one provider call attempt.

- [ ] **Step 4: Audit secrets and repository state**

Run repository-wide searches for real credential formats and inspect staged/untracked files. Confirm `.env` is ignored, examples contain placeholders only, both repositories have clean diffs after commits, and no transcript/audio logging was introduced.

- [ ] **Step 5: Record verification without overstating deployment**

Update both READMEs with the exact passing local commands. State separately that production Supabase migration, self-hosted Redis connectivity, GitHub remote creation/push, server deployment, and a paid outbound call have not been proven unless they were actually performed.

- [ ] **Step 6: Commit final documentation separately**

Core commit: `docs: add Vox Core operations guide`

Bridge commit: `docs: document Core integration`
