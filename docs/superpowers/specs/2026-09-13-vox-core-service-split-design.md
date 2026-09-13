# Vox Core Service Split Design

## Goal

Split Vox into two deployable services without fragmenting the product into per-stage microservices:

- `vox-bridge` owns external communication protocols and provider-specific delivery.
- `vox-core` owns identity, memory, agents, events, scheduling, planning, actions, and durable workflow state.

The first vertical slice moves the Gemini agent and its Exa and Google Maps tools from Bridge into Core, wires live voice turns through Core, persists conversations and summaries in Supabase Postgres, uses self-hosted Redis for hot context and worker wake-ups, and supports durable scheduled or event-triggered outbound-call actions.

## Repository and Deployment Shape

The system has exactly two application repositories:

1. The existing `vox-bridge` repository remains at `/Users/rahul/Documents/vox/vox-bridge`.
2. A new sibling Git repository is created at `/Users/rahul/Documents/vox/vox-core`.

`vox-core` is a modular Rust application with a shared library and two binaries:

- `vox-core-api` serves internal HTTP requests from Bridge.
- `vox-core-worker` processes durable jobs, schedules, summaries, and actions.

Each repository owns its Dockerfile. `vox-core/compose.yaml` is the development and single-server composition and builds both repositories. It runs Bridge, Core API, Core Worker, and Redis on one private Docker network. Supabase Postgres remains external and is reached through `DATABASE_URL`; the composition does not run a competing production database.

The two Core binaries use one image with different commands. Redis uses append-only persistence and a named volume. Only Bridge is published to the host. Core API and Redis are private to the Docker network. Bridge waits for Core API readiness. API and Worker require Postgres but tolerate Redis being temporarily unavailable; Worker does not depend on the API process.

No credentials are committed. Each repository includes an `.env.example` containing names and safe placeholder values only. Runtime secrets are supplied through an ignored `.env` or the server's secret management.

## Responsibility Boundaries

### Vox Bridge

Bridge owns:

- Twilio and WhatsApp webhook validation
- Twilio inbound and outbound call initiation
- bidirectional call media streaming
- AssemblyAI STT and Sarvam TTS
- provider-specific request and response formats
- mapping external identities into a normalized channel identity
- forwarding finalized text turns to Core
- delivering actions requested by Core

Bridge does not own LLM prompts, tools, user memory, summaries, event planning, schedules, or durable action state.

### Vox Core API

Core API owns latency-sensitive and request-facing application operations:

- resolving a stable Vox user from a channel identity
- opening or resuming a conversation
- loading bounded memory from Redis with Postgres fallback
- persisting user and assistant turns
- running the conversation agent and its tools
- accepting normalized external events from Bridge
- creating and managing scheduled tasks
- receiving action results from Bridge
- health and readiness reporting

### Vox Core Worker

Core Worker owns asynchronous operations:

- claiming durable jobs
- processing new external events
- running the event-planning agent
- materializing due schedules into jobs
- dispatching pending actions to Bridge
- retrying failed work with bounded backoff
- producing post-conversation structured summaries
- refreshing Redis context after durable state changes
- recovering expired job leases after a crash

API and Worker share domain and application modules. They are separate processes so background failures and expensive work cannot block live conversations.

## Core Module Layout

`vox-core` uses one Cargo package with multiple binaries:

```text
src/
  lib.rs
  bin/
    vox-core-api.rs
    vox-core-worker.rs
  config.rs
  http/
  identity/
  conversations/
  memory/
  agents/
  events/
  schedules/
  jobs/
  actions/
  summaries/
  db/
  cache/
  bridge_client/
migrations/
```

Modules expose narrow application interfaces. HTTP handlers parse and validate transport input, then call application services. Database, Redis, Gemini, and Bridge clients are adapters behind traits so tests replace network boundaries without paid calls.

## Identity and Conversation Contract

Core uses an internal UUID `user_id`. External identifiers are stored separately so phone, WhatsApp, and future web identities can resolve to the same user.

Bridge sends a normalized identity with each call turn:

```json
{
  "channel": "phone",
  "external_id": "+919876543210"
}
```

An initial unresolved identity creates a user and identity mapping. A later account-linking flow may merge identities, but automatic matching across different identifiers is excluded.

Each provider interaction has a stable `conversation_id`. For Twilio calls, Bridge uses the Call SID as the external conversation identifier. Core maps it to its own conversation row and preserves ordered user and assistant messages.

## Bridge-to-Core API

All internal requests use JSON over HTTP on the private Docker network and carry a shared service bearer token. This token protects service-to-service traffic and is separate from future user permission policy.

### Conversation turn

`POST /v1/conversations/respond`

Request fields:

- channel identity
- external conversation ID
- user text
- optional initiation context for an outbound call

Core resolves identity, loads memory, stores the user turn, invokes the agent, stores the assistant turn, and returns the response text. The current Bridge waits for a complete text response before TTS, so the initial contract is ordinary JSON rather than token streaming.

### Event ingestion

`POST /v1/events`

The request contains a caller-supplied idempotency key, channel identity, event type, occurrence time, and JSON payload. Core inserts the event and its processing job in one Postgres transaction and returns `202 Accepted`. Repeating the same idempotency key returns the existing event without creating another job.

### Schedule management

`POST /v1/schedules` creates a one-time or recurring schedule. `PATCH /v1/schedules/{id}` pauses, resumes, or changes it. Times are stored as UTC instants; a recurrence also stores the user's IANA timezone so the next occurrence remains correct across clock changes.

### Action result

`POST /v1/actions/{id}/result` records the provider identifier and sanitized success or failure result. Duplicate results are idempotent.

## Core-to-Bridge API

Bridge exposes a private endpoint for Core:

`POST /internal/v1/actions/outbound-call`

The request contains:

- Core action ID as the idempotency key
- destination channel identity
- reason and opening instruction
- Core conversation identifier

Bridge uses the Twilio Calls API to initiate the call, registers the returned Call SID before media begins, and returns the provider call identifier. Repeating an already accepted Core action returns the original provider identifier rather than placing another call.

When the outbound media stream connects, Bridge asks Core for the opening response using the supplied initiation context and plays it before waiting for user speech. Subsequent finalized turns use the same conversation endpoint as inbound calls.

## Agent Migration

The Gemini agent, Exa search tool, Google Maps tools, tool dependencies, prompts, and model configuration move from `vox-bridge` to `vox-core`.

Bridge replaces its local Gemini implementation with `CoreAgentClient`, an implementation of the existing agent-provider boundary. The voice session supplies call context alongside each transcript so the Core request includes channel identity and conversation identity.

Core has three explicit agent responsibilities built on shared Gemini and tool adapters:

- `ConversationAgent` produces the next conversational response.
- `EventPlanner` converts a stored event plus current user context into zero or more structured actions.
- `Summarizer` produces the structured post-conversation summary and profile updates.

Agent outputs that affect workflow are parsed into versioned typed structures. Invalid structured output fails the job and follows normal retry handling; it is never executed as an unvalidated free-form instruction.

## Durable Data Model

Supabase Postgres is the system of record. The initial migrations create:

- `users`
- `user_identities`
- `conversations`
- `messages`
- `user_profiles`
- `conversation_summaries`
- `events`
- `scheduled_tasks`
- `jobs`
- `actions`
- `action_attempts`

Rows use UUID primary keys and UTC timestamps. JSON is limited to external event payloads, versioned structured agent output, and provider metadata; lifecycle state and query-critical values use typed columns.

The `jobs` table provides the durable queue. A job records its type, payload reference, state, attempt count, next-attempt time, lease owner, and lease expiry. Workers claim eligible jobs using a short transaction and `FOR UPDATE SKIP LOCKED`. Expired leases become claimable again. Job handlers and action delivery are idempotent.

Schedules remain durable definitions rather than pre-created future jobs. The scheduler claims due rows, creates one job per occurrence, and advances `next_run_at` in one transaction. A uniqueness constraint on schedule and occurrence prevents duplicate execution.

## Redis Role

Redis is an optimization, not the source of truth. It stores:

- bounded precomputed user context
- short-lived conversation context
- worker wake-up notifications
- rate and cooldown state needed on the hot path

A missing key is rebuilt from Postgres. If Redis is unavailable, Core continues using Postgres with higher latency. Durable events, jobs, schedules, summaries, and actions never exist only in Redis.

Kafka is not part of this design. Postgres job claiming is sufficient for the initial event rate and preserves a much smaller operational surface.

## Memory and Summary Lifecycle

Each completed conversation creates one immutable structured summary containing:

- concise interaction recap
- new or changed profile facts
- commitments and pending follow-ups
- decisions and meaningful outcomes

Profile facts are merged into current state rather than appended forever. Redis receives a bounded context projection containing current profile facts, active commitments, recent summaries, and compact older milestones. Full messages and every per-conversation summary remain queryable in Postgres.

The first implementation compacts by a fixed context budget rather than calendar-based daily, monthly, and quarterly summarization. Older summaries fall out of the hot projection but remain durable. Hierarchical historical roll-ups can be added when real usage demonstrates that recency plus active commitments is insufficient.

## Event, Schedule, and Action Flow

### External event

1. An external source sends an authenticated provider event to Bridge.
2. Bridge verifies it and converts it into the normalized event contract.
3. Core stores the event and processing job atomically.
4. Worker loads the user context and asks `EventPlanner` for structured actions.
5. Valid actions are stored before dispatch.
6. The executor sends an outbound-call action to Bridge.
7. Bridge initiates the provider call and returns its identifier.
8. Core records the attempt and follows the resulting conversation.

### Scheduled task

1. API stores the schedule and `next_run_at`.
2. Worker claims schedules that are due.
3. The scheduler creates a unique occurrence job and advances the schedule.
4. The planner or deterministic handler produces an action.
5. The normal action executor delivers it through Bridge.

The same action pipeline handles event-driven and time-driven work.

## Failure Handling

- API writes return success only after Postgres commits.
- Worker jobs have bounded attempts with exponential backoff and a terminal failed state.
- Leases allow another worker to recover work after a crash.
- Event, schedule occurrence, action, and Bridge delivery idempotency prevent duplicate calls.
- A Core timeout causes Bridge to skip the affected response while keeping the call alive when the audio connection remains usable.
- Provider failures are sanitized before persistence and never include credentials or raw sensitive response bodies.
- Post-call summary failure does not erase the transcript; it remains retryable.
- Health endpoints distinguish process liveness from Postgres and Redis readiness.

## Docker and Local Operation

Both repositories use multi-stage builds and run as non-root users in minimal runtime images. Build caches are separated from runtime layers, and dependency lockfiles are honored.

`vox-core/compose.yaml` defines:

- `bridge`
- `core-api`
- `core-worker`
- `redis`

The Core image runs its API and Worker binaries as separate containers. Redis enables append-only persistence in a named volume. Services share an internal network; Bridge alone binds the application port to the host. Compose waits for Core API before starting Bridge, while API and Worker connect independently to Postgres and treat Redis as optional. Restart policies recover failed processes, and graceful shutdown gives calls and leased jobs bounded cleanup time.

The Compose file accepts an external `DATABASE_URL` for Supabase. Automated tests use isolated local test boundaries and do not require paid provider calls. A separate local Postgres profile is excluded from the first setup to avoid presenting a local schema as equivalent to Supabase configuration.

## Testing and Verification

### Core

- identity resolution is stable and channel-scoped
- duplicate event ingestion creates one event and one job
- concurrent workers claim a job once
- expired leases are recoverable
- schedule occurrences are unique and recurring times advance correctly
- event-planner output must match the typed action schema
- summaries persist before Redis refresh
- Redis misses fall back to Postgres
- action retries do not duplicate Bridge delivery
- API and Worker start independently

### Bridge

- voice turns include correct identity and conversation context
- Core errors do not leak internal response bodies
- agent responses now come from the Core client
- outbound action requests are authenticated and idempotent
- Twilio outbound call state is registered before stream acceptance
- outbound calls play the Core-generated opening response
- existing inbound call, barge-in, STT, TTS, webhook, and signature tests continue to pass

### Composition

- both images build from lockfiles
- all four containers become healthy with test configuration
- Core API is not published to the host
- stopping Redis does not lose durable work
- restarting Worker recovers an expired leased job
- one mocked event produces one mocked outbound-call request end to end

Final local verification consists of formatting, full tests, strict Clippy, release builds, Docker image builds, Compose configuration validation, container health checks, and the mocked end-to-end event flow. Live deployment, Supabase provisioning, Redis server installation outside Docker, GitHub repository publication, production secrets, and paid Twilio outbound calls require their corresponding external environments and are reported separately rather than implied by local success.

## Scope Exclusions

This implementation does not add Kafka, split Core modules into independent network services, create a user-facing permissions system, implement account identity merging, add semantic transcript RAG, add calendar-based summary roll-ups, or deploy production secrets. It establishes durable boundaries so those capabilities can be added without changing Bridge's external-provider role or Core's ownership of intelligence and state.
