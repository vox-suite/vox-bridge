# Vox Bridge Reorganization Implementation Plan

> **For agentic workers:** Use `superpowers:executing-plans` to implement this plan task by task. Preserve existing worktree changes. This document authorizes no deployment or cross-repository implementation.

**Goal:** Make Vox Bridge a focused communication-provider adapter with a testable real-time voice runtime, preserving current channel contracts and separating cleanup from latency experiments.

**Architecture:** Keep one Bridge binary and one Rust crate. Bridge owns Twilio/WhatsApp transport, STT/TTS, audio processing, and ephemeral call state; Core owns identities, agents, tools, persistence, schedules, connector credentials, and desktop execution routing. Mobile/desktop application APIs go to Core through deployment ingress, not through Bridge.

**Tech stack:** Existing Rust 2024, Tokio, Axum, reqwest, WebSockets, SSE, AssemblyAI, Sarvam/ElevenLabs, and existing ONNX audio-feature extraction. No new service, database, queue broker, provider, or framework is required for the reorganization.

**Spec:** Architecture proposed in the preceding conversation; supplied [VOICE_PIPELINE.md](../../../../VOICE_PIPELINE.md) and [VOICE_LATENCY_OPTIMIZATION.md](../../../../VOICE_LATENCY_OPTIMIZATION.md). The reconciliation below defines how their voice requirements apply to Bridge. These relative references resolve in the current multi-repo workspace; Task 10 makes the maintained Bridge documentation self-contained.

## Global constraints

- Preserve existing edits and deletions; do not reset, restore, stage, or commit unrelated work.
- No new comments in implementation code; document decisions in Markdown and meaningful type/function names.
- Keep STT, TTS, and telephony independently replaceable. Preserve the current provider default during structural changes; current source defaults to ElevenLabs and supports Sarvam.
- No direct database/Redis access, prompts, business tools, shopping connectors, or desktop job scheduling in Bridge.
- Speculative partial turns initiate read-only work in Core. Finalized complete turns trigger the main agent in Core.
- Greeting cache lookup, user creation, name resolution, and speaker identity decisions stay in Core. Bridge does not extract biometric evidence or assign an active speaker.
- Preserve channel-prefixed identities, signed host assertions, and existing HTTP paths while moving code.
- Preserve current voice behavior first. Put deliberate behavior changes in separate reviewable changes with regression tests.
- No paid provider calls, production mutations, model downloads, source pushes, or releases as part of preparing this plan.

## Updated voice scope from user clarification

The user states local VAD and speaker verification have been removed for now. This supersedes earlier inspection-derived biometric/VAD work below. Do not restore them: remove residual acoustic-signature generation, model startup/downloads, voiceprint payloads, speaker switching, and unused ONNX/FFT dependencies after checking remaining consumers. Keep STT/provider speech/turn signals and test interruption/turn settling through the selected supported signals; provider-internal endpointing is distinct from a local VAD module. Generic greetings must not depend on biometric identity.

Retain provider signature verification, channel identity, Core request authentication, and bounded media/MP3 conversion. Update Tasks 5, 7, 8, and 10 accordingly; no embedding benchmark or model-backed packaging smoke test is required. If an old Core payload temporarily accepts a nullable voice_signature, omit it or send null until both sides retire the field. The previous baseline is historical; this planning update does not verify or modify current runtime code.

## Current evidence, inspected 2026-09-22

Baseline: Bridge HEAD `085eb58`, plus an already-modified `src/voice/session.rs`. Three `.github/workflows/*.yml` files and four earlier design/plan documents are already deleted in the worktree.

Executed `cargo test --locked --quiet` in `vox-bridge`: **77 unit tests passed; one integration test failed**. `tests/publish_workflow.rs::validates_release_and_hands_deployment_to_orchestrator` fails because `.github/workflows/publish.yml` is missing. Formatting, Clippy, release build, containers, and live calls were not verified in this planning pass. The historical `config.rs` syntax failure is not present in this baseline.

| Evidence | Consequence for this plan |
| --- | --- |
| `src/agents/mod.rs` exports Core client, STT, and TTS; no business agent | Rename by responsibility; do not invent an agent extraction project |
| `src/agents/jev_client.rs` is not declared in the module tree and has no discovered consumers | Remove after a full reference check; do not enable a second Jev integration |
| `src/voice/session.rs` is 1,474 lines including tests | Extract response streaming, speculation, playback state, and metrics incrementally |
| `src/main.rs` builds state, providers, routes, an unused broadcast path, and warmup logic | Separate bootstrap, configuration, state, and router |
| WhatsApp obtains its Core client through the voice provider registry | Decouple text delivery from STT/TTS initialization |
| `AgentProvider`, `CoreAgentClient`, and `ProviderRegistry.agents` imply interchangeable local agent engines | Replace with one injected Core conversation interface |
| `DraftTurn::settle_delay()` is 350 ms normally and 1,200 ms for numeric fragments | Preserve fragment correctness; do not promise sub-500 ms through moves alone |
| Filler currently waits 400 ms after `LookupPending`; docs specify 800 ms after speech end | Preserve in structural phase; adjust only in the timing phase |
| Voice onset currently clears playback; no active semantic Jev backchannel arbitration | Treat intelligent arbitration as future behavior, not something the refactor already preserves |
| `spawn_response` awaits acoustic extraction before requesting Core | Measure this critical-path cost; do not bypass identity safeguards to improve latency |
| ElevenLabs runtime defaults to MP3 and includes conversion code | README claim of universally avoiding transcoding needs correction; retain decoder |
| `AppState.service_token` is initialized to an empty string; outbound handler and result callback use it | Outbound action auth is a correctness gap, separate from conversation host auth |
| Status callback posts `/v1/actions/{id}/result`; no matching route found in current Core router | Outbound completion requires an explicit Core contract decision before claiming it works |
| ONNX test conditionally runs assertions only when a model loads | Passing unit tests do not demonstrate real model availability or inference compatibility |

## Ownership and document reconciliation

| Concern in supplied documents | Bridge responsibility | Core/deployment responsibility or correction |
| --- | --- | --- |
| Twilio initiation, signature verification, streaming | Validate provider input, normalize events, manage media session | Gateway routes `/bridge/*` to Bridge |
| Greeting before conversation | Request and play opening on call connection | Core cache-only lookup with 100 ms cache deadline and generic fallback; no database wait on opening path |
| Diagram connects STT to greeting | Opening is independent of receiving a caller transcript | Correct the diagram; do not wait for speech to greet |
| Redis `vox:greeting-names`, `COMMAND_CREATE_USER` | No Redis client or user-creation command | Core owns cache and asynchronous identity initialization |
| Jev speculation and “DB Get / Set” | Forward bounded partial snapshots with turn ID and revision | Core owns Jev and read-only retrieval; remove speculative writes from specification |
| Fillers and immediate acknowledgments | At most one pending-lookup filler, cancellable and ordered with answer audio | Core emits lookup state; do not add routine acknowledgments to turns without pending lookups |
| Prompt/history assembly and LLM selection | Consume Core text/event stream | Core owns all reasoning and tool policy |
| Punctuation chunking and TTS | Bridge-owned output pacing and format normalization | Provider changes are independent experiments |
| Biometrics | Bounded audio buffer and acoustic feature extraction | Core performs comparison, verification, enrollment, and identity changes |
| Intelligent barge-in | Local interruption and playback control cannot wait on remote model calls | Semantic classifier is optional future work; any Core hint must have a bounded local fallback |
| 20% volume ducking | Possible only for audio still under Bridge control | Do not claim control over already buffered provider audio; defer until transport feasibility and audible behavior are tested |
| 440 ms projection, provider WER/TTFT, human abandonment thresholds | Treat as research hypotheses, not acceptance evidence | Require sourced and reproducible measurements before adopting numbers |
| “Mumbai media edge”, VPC peering, PrivateLink | No topology assumptions in code | Verify actual product/region support; Twilio's current Media Streams page lists US1, IE1, AU1 |

Twilio marks can acknowledge completed **or cleared** buffered audio. A mark after `clear` is not proof that the caller heard the response. Keep a record of invalidated playback generations. Reference: [Twilio Media Streams WebSocket messages](https://www.twilio.com/docs/voice/media-streams/websocket-messages).

The latency document includes an arXiv-style reference containing `2608.latency-floor-cascade-voice-agents`, which is not a conventional numeric arXiv identifier. Its benchmark and research claims have not been independently validated here. Do not turn these citations into implementation requirements.

## Target structure and dependency direction

```text
src/
  main.rs                         process entry; call application bootstrap
  lib.rs                          expose router/runtime to integration tests
  app.rs                          construct clients, runtime, listener, shutdown
  config.rs                       channel settings, validated URLs and credentials
  state.rs                        Core client and enabled channel state
  http/
    mod.rs                        compose existing routes
    health.rs                     liveness/readiness
    internal.rs                   authenticated outbound-call entry point
  channels/
    mod.rs
    context.rs                    normalized channel identity/context
    twilio/
      mod.rs
      webhook.rs                  current twilio_post.rs
      stream.rs                   socket orchestration
      protocol.rs                 Twilio wire structs and serialization
      signature.rs                signature canonicalization/verification
      status.rs                   current twilio_status.rs
      client.rs                   current telephony/twilio_client.rs
    whatsapp/
      mod.rs
      webhook.rs                  verify/receive normalized messages
      client.rs                   outbound WhatsApp delivery
  core/
    mod.rs                        ConversationClient injection interface
    client.rs                     HTTP request execution
    auth.rs                       existing host assertion signing
    protocol.rs                   existing request/response/event shapes
    sse.rs                        streaming parser
  providers/
    mod.rs
    stt/{mod.rs,assemblyai.rs}
    tts/{mod.rs,sarvam.rs,elevenlabs.rs}
    telephony.rs                  existing TelephonyClient contract
  voice/
    mod.rs
    config.rs                     voice timing/profile configuration
    provider.rs                   audio/STT/TTS traits and errors only
    registry.rs                   STT/TTS construction; injected Core client
    session/{mod.rs,response.rs,speculation.rs,playback.rs,tests.rs}
    turn.rs                       DraftTurn assembly and settling
    chunker.rs                    linguistic text boundaries
    filler.rs                     cached filler synthesis/playback
    metrics.rs                    timing records and trace emission
    audio/{mod.rs,mp3.rs}
tests/
  fixtures/                       existing fixture plus synthetic protocol cases
  core_contract.rs
  channel_routes.rs
  voice_replay.rs
  publish_workflow.rs              retain or replace with verified release ownership
docs/
  architecture.md
  voice-pipeline.md
  voice-latency.md
  contracts/core.md
```

Dependency direction: `app/http -> channels -> voice/Core interfaces -> concrete injected clients`. Provider implementations use voice traits. Core HTTP/signing/parsing must not depend on concrete STT/TTS or Twilio internals. Voice must not import Twilio wire JSON. Do not make one crate per folder.

## Delivery sequence

Execute Tasks 1–6 as the structural track. Tasks 7–9 are separate correctness/performance changes. Task 10 closes documentation and release verification. A cross-repo dependency must not force unrelated structural work to stop.

### Task 1: Establish a reproducible baseline and contract inventory

**Files:** create `docs/contracts/core.md`; preserve `src/voice/session.rs`, `tests/publish_workflow.rs`, and existing workflow deletions.

- [ ] Record `git status --short`, `git diff --stat`, and `git rev-parse HEAD`. Save test results without environment values or secrets.
- [ ] Run the commands below and record failures individually. Do not silently restore deleted workflows or delete their test just to get green.

```sh
cargo test --locked --bin vox-bridge
cargo test --locked --test publish_workflow
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
```

- [ ] Inventory four existing conversation endpoints: `/v1/conversations/respond`, `/respond/stream` under the same prefix, `/v1/conversations/speculate`, `/v1/conversations/complete`. Record host headers, channel identity, `external_conversation_id`, `turn_id`, `revision`, `voice_signature`, `tts_provider`, SSE deltas, `lookup_pending`, stream end, errors, and unary fallback restricted to 404/405.
- [ ] Record outbound-call/result contracts separately; current result endpoint is not implemented in the inspected Core router. Identify release ownership from `../vox-deploy/README.md`, `scripts/release.sh`, and tests before proposing workflow changes.
- [ ] Preserve all existing behavior tests. Capture current timer values and provider defaults in the contract document.

**Deliverable:** known baseline with explicit pre-existing failures and a contract checklist. No claim of a green full suite until the workflow mismatch is resolved.

### Task 2: Extract application bootstrap, config, and routes

**Files:** modify `src/main.rs`; create `src/lib.rs`, `src/app.rs`, `src/config.rs`, `src/state.rs`, `src/http/{mod.rs,health.rs,internal.rs}`; move route composition unchanged.

**Interfaces:** `pub fn router(state: Arc<AppState>) -> axum::Router`; `pub async fn run() -> Result<(), Box<dyn std::error::Error>>`. `AppState` retains existing fields until their consumers are migrated.

- [ ] Move existing router construction into `router`, startup into `run`, and change binary entry to invoke `vox_bridge::app::run().await`. Keep bind address and routes identical.
- [ ] Move existing handler tests with imports adjusted; preserve signature rejection and malformed-input assertions.
- [ ] Add `tests/channel_routes.rs` with local router/server tests: health succeeds; unsigned Twilio webhook is rejected; unsigned stream upgrade is rejected; unknown route returns 404. Never put real credentials in fixtures.
- [ ] Add typed configuration for canonical public callback base URL and enabled channels. Default public URL to the existing address for compatibility; validate configured HTTPS/WSS URL shape. Test canonical signature input including status query parameters before replacing hardcoded constants.
- [ ] Keep warmups best-effort and independent of readiness; introduce graceful shutdown which stops accepting new sessions and drains active ones with a deadline.
- [ ] Run `cargo test --locked --lib`, `cargo test --locked --test channel_routes`, and `cargo check --locked` after each move.

**Deliverable:** testable startup/router without changing public channel paths or signing semantics.

### Task 3: Make the Core boundary explicit

**Files:** move `src/agents/core_client.rs` into `src/core/{client.rs,auth.rs,protocol.rs,sse.rs}`; move `src/agents/core_client_test.rs` into Core module tests or `tests/core_contract.rs`; move `src/voice/context.rs` to `src/channels/context.rs`; modify `src/voice/provider.rs`, `registry.rs`, and consumers.

**Interfaces:** rename `AgentProvider` to `ConversationClient`, `CoreAgentClient` to `CoreClient`, `AgentEvent` to `ConversationEvent`, and `AgentEventStream` to `ConversationEventStream`. Preserve the current async method arguments and return types for `respond`, `respond_stream`, `respond_events`, `speculate`, and `complete`, using the renamed event types. `CallContext` remains its existing shape during this move.

- [ ] Extract signing and SSE parser without changing bytes on the wire. Keep existing host assertion secret handling and channel prefixes intact; credential-protocol redesign is out of scope.
- [ ] Replace untyped repeated JSON construction with serde request structs matching existing payload fields and null/omission behavior. Compare serialized fixtures against current requests.
- [ ] Move the conversation trait/event types into `core`; inject `Arc<dyn ConversationClient>` directly. Remove the single-entry agents registry only after all callers use this dependency.
- [ ] Keep `ProviderSet` for STT/TTS and the injected conversation client; do not replace one simple struct with a dynamic plugin framework.
- [ ] Extend Core contract tests for split UTF-8/SSE boundaries, CRLF, multiple events per packet, final buffer flush, malformed payloads, unauthorized response, 404/405 fallback, and no retry/fallback after partial response delivery.
- [ ] Run `cargo test --locked core` and `cargo test --locked --test core_contract`. New fixtures use only a local fake Core server.

**Deliverable:** Bridge has a clearly named Core transport boundary with wire compatibility demonstrated by fixtures.

### Task 4: Group channel and provider adapters; remove dead scaffolding

**Files:** move `src/routes/twilio/*` to `src/channels/twilio/*`; move `src/telephony/*` to `src/providers/telephony.rs` and `src/channels/twilio/client.rs`; move `src/agents/stt/*` and `src/agents/tts/*` to `src/providers`; split `src/routes/whatsapp.rs`; modify bootstrap/state.

- [ ] Move provider implementations and their existing tests one provider at a time; retain current models, output formats, and defaults.
- [ ] Extract Twilio protocol structs and serializer into `protocol.rs`, and shared signature validation into `signature.rs`; handlers retain their route-specific canonical URL construction.
- [ ] Give WhatsApp direct access to the shared Core client and a reusable configured HTTP delivery client. WhatsApp-only startup must not demand AssemblyAI, TTS, or a speaker model.
- [ ] Replace request-time environment panics in WhatsApp verification/delivery with validated startup configuration for enabled channels. Missing challenge is a bad request; missing signature remains rejected; a non-success provider delivery response must become an error.
- [ ] Search references before deleting the uncompiled `agents/jev_client.rs`, root Hello page, `/ws` broadcast handler, and `AppState.tx`. Search sibling clients and deployment routes as well; migrate an actual consumer before removing its endpoint. This plan adds no replacement generic device socket.

```sh
rg -n 'BridgeJevClient|jev_client|broadcast|state\.tx' src tests
rg -n '/ws|api\.voxagent\.in' ../vox-desktop/src ../vox-web/src ../vox-deploy
cargo test --locked --lib
cargo test --locked --test channel_routes
```

**Deliverable:** channel adapters and provider integrations are separate from Core intelligence, and text channels do not depend on voice setup.

### Task 5: Decompose voice orchestration without behavior changes

**Files:** split `src/voice/session.rs` into `session/{mod.rs,response.rs,speculation.rs,playback.rs,tests.rs}`; create `voice/metrics.rs`; move `mp3.rs` under `voice/audio/`; retire unused local VAD/embedding modules; update imports.

**Interfaces:** preserve `run_voice_session(ProviderSet, CallContext, mpsc::Receiver<CallEvent>, mpsc::Sender<CallCommand>) -> Result<(), VoiceError>` and existing `CallEvent`/`CallCommand` variants in this task. `session/mod.rs` re-exports them. Internal helpers use `pub(super)` as needed.

- [ ] Move `TurnTiming` and `log_turn_latency` to metrics; preserve field meanings and timestamps.
- [ ] Move `spawn_response`, `stream_response`, and `play_sentence` to response; preserve filler ordering, chunking, deadlines, and cancellation behavior.
- [ ] Move the debounced watch-channel speculation task to speculation; keep 250 ms debounce and latest-snapshot replacement.
- [ ] Encapsulate existing response/playback state in playback without changing decisions; keep the session loop responsible for transitions.
- [ ] Move all existing session tests intact to `session/tests.rs`; retain the current user's session edits as the input to extraction.
- [ ] Retire unused local VAD/embedding paths under the updated scope. Preserve MP3 conversion fixture coverage and STT-event turn/interrupt regression tests.

```sh
cargo check --locked
cargo test --locked voice::session
cargo test --locked voice::turn
cargo test --locked voice::audio
```

Run check and the relevant test filter after each extraction, then the full unit suite once the moves are complete.

**Deliverable:** smaller modules with the same observed event sequences and existing behavior tests retained.

### Task 6: Create an offline end-to-end replay gate

**Files:** create `tests/voice_replay.rs` and `tests/fixtures/voice/`; extend local fake providers already used by session tests; expose only necessary library interfaces.

- [ ] Build a replay harness around `run_voice_session` with synthetic audio and scripted STT/Core/TTS streams. Capture commands and ordered timestamps; tests must not need vendor credentials.
- [ ] Add cases for opening without caller speech, fragmented phone number, revised partials, rapid consecutive turns, slow Core, pending lookup, interruption during filler, stale mark, provider disconnect, and call stop.
- [ ] Assert Core sees one finalized assembled numeric turn, updated revisions replace partial snapshots, filler appears at most once only after pending state, and call teardown releases all channel senders/tasks.
- [ ] Record slow-stream behavior rather than pretending synthetic timings prove real vendor latency. Keep test fixtures free of real phone numbers, transcripts, and voiceprints.
- [ ] Run `cargo test --locked --test voice_replay` against the structural refactor before beginning timing changes.

**Deliverable:** reproducible voice behavior baseline independent of paid APIs.

### Task 7: Harden playback invalidation and resource cleanup

**Files:** modify `voice/session/{mod.rs,playback.rs,response.rs,speculation.rs}`, `channels/twilio/stream.rs`, replay tests.

**Proposed internal interface:** `PlaybackState::begin() -> u64`, `invalidate() -> u64`, `accepts(generation: u64) -> bool`. Tag queued output with a generation using `PlaybackCommand { generation: u64, command: CallCommand }`. Keep provider wire JSON unchanged.

- [ ] Add a failing playback unit test using the proposed interface:

```rust
#[test]
fn invalidation_rejects_old_output() {
    let mut playback = PlaybackState::default();
    let old = playback.begin();
    assert!(playback.accepts(old));
    playback.invalidate();
    assert!(!playback.accepts(old));
}
```

- [ ] On interruption invalidate the old generation, cancel its response/TTS work, discard stale queued output, and prioritize Clear before new-generation media. The Twilio writer checks generation immediately before sending. Already-sent media is handled by Clear; test the send/invalidation race.
- [ ] Track marks against their generation and whether they were cleared. A returned cleared mark cannot end a newer response or be logged as proof of audible completion.
- [ ] Make shutdown cleanup run on success, errors, and cancellation. Aborting the parent must not detach STT/speculation/input tasks; abort and join owned handles with explicit deadlines. Bound blocking embedding concurrency because aborting an async waiter does not stop started blocking work.
- [ ] Reserve control-event capacity or use a separate bounded control channel so an audio backlog cannot lose Stop/Clear/mark processing. Preserve explicit overflow failure; do not silently drop speech frames.
- [ ] Test saturated queues, canceled TTS still producing a late chunk, clear followed by old mark, missing marks, stream errors, and repeated stop events. Add a bounded missing-mark recovery policy so playback never stays active indefinitely.
- [ ] Run session, Twilio, and replay suites. Drop of a Core HTTP stream must be described as local cancellation only; remote LLM/tool cancellation requires a separate Core contract and is not assumed.

**Deliverable:** cancellation prevents stale playback and session termination does not leak work.

### Task 8: Measure and reconcile latency policy

**Files:** modify `voice/config.rs`, `voice/metrics.rs`, `voice/session/response.rs`, `voice/turn.rs`, `voice/chunker.rs`, `docs/voice-latency.md`; extend replay tests.

- [ ] Introduce explicit settings retaining current defaults for normal settle (350 ms), numeric settle (1,200 ms), and speculation debounce (250 ms). Do not change endpointing based on a projected total.
- [ ] Record separate durations: call-connect to opening first media; last speech to final transcript; final transcript to dispatch; embedding duration; Core connect/first text; chunk wait; TTS first audio; output queue wait; first answer media; mark acknowledgment; interrupt decision to Clear send.
- [ ] Correlate by call, turn, revision, and playback generation. Remove raw transcript/full-response/voiceprint logging from routine production traces; record counts and timing instead. Never label socket-send time as handset audible time.
- [ ] Implement the document's pending-lookup filler deadline as `speech_end + 800 ms`, only while pending and before answer text. If pending arrives after that deadline, emit once immediately if still relevant. Suppress it when the answer wins the race. Treat absent speech-end timing explicitly: use final-transcript receipt as a labeled fallback, not a fabricated speech timestamp.
- [ ] Write fake-clock tests for no pending lookup, pending at 200 ms, pending after 800 ms, answer arriving at 790 ms, cancellation before deadline, and duplicate pending events. Add Tokio test-util only as a dev feature if needed. Keep one ordered output writer.
- [ ] Benchmark current chunking before changing it. If testing a 6–8-word flush, preserve abbreviations, decimal numbers, Unicode text, incomplete tokens, and final flush; compare time to first answer audio and spoken phrase quality. Ship chunking adjustment separately from the timer change.
- [ ] Measure whether serialized TTS playback stalls Core stream consumption. Only if demonstrated, split text consumption from synthesis with a bounded clause queue and ordered output; test cancellation and backpressure together.
- [ ] Report p50/p95 separately for greetings, ordinary turns, numeric capture, and tool-backed turns. Use a same-machine replay comparison and a controlled staging-call sample; proposed regression gate is no more than 10% or 50 ms (whichever is larger) additional Bridge-owned p95 processing time under equivalent load. Report sample size and variance.

**Deliverable:** attributable latency and tested filler policy. Sub-500 ms remains a stretch objective, not a condition claimed satisfied by cleanup. Provider swaps, semantic endpointing, prompt caching, and regional hosting remain separate measured experiments.

### Task 9: Resolve channel delivery contracts without moving durable state into Bridge

**Files:** Bridge `config.rs`, `http/internal.rs`, `channels/twilio/{status.rs,client.rs}`, `channels/whatsapp/{webhook.rs,client.rs}`, `core/protocol.rs`, contract tests/documentation. **Dependent repositories:** Core HTTP/action execution modules and deployment credential wiring require a separate coordinated change.

- [ ] Fail closed on absent/empty outbound action credentials. Keep conversation host assertions separate from action authority. Do not restore a broad Core service token as an undocumented workaround.
- [ ] Define a scoped outbound-action contract with Core before enabling the path: authenticated dispatch identity, action ID, provider call ID, allowed recipient, terminal result, and replay rules. Record exact endpoint/auth agreement in `docs/contracts/core.md`; current `/v1/actions/{id}/result` must not be assumed operational.
- [ ] Until that contract is implemented, disable outbound actions explicitly and return 503 rather than accepting calls whose results cannot be persisted. Keep inbound calls independently operational.
- [ ] Put durable action deduplication, retries, unknown-outcome reconciliation, and terminal-state transitions in Core. Bridge's in-memory scan cannot guarantee exactly-once calls across concurrency or restarts; do not automatically reissue a provider create-call request after an ambiguous timeout.
- [ ] Map status callbacks by provider lifecycle: intermediate statuses must not become failed actions; only defined terminal statuses finalize. Test bad signatures, duplicate terminal callbacks, out-of-order callbacks, unknown calls, and Core callback failure.
- [ ] For WhatsApp, include provider message ID in a future durable-ingestion contract. A fast acknowledgment is safe only after durable acceptance by Core; do not replace synchronous processing with an untracked Tokio spawn. Keep that protocol extension as a separate Core-dependent change.
- [ ] Acceptance requires local contract tests in both repos and a coordinated staging flow. Bridge structure can finish while these capabilities remain explicitly disabled or documented as blocked.

**Deliverable:** honest supported-channel boundaries; no accidental second scheduler/database or unreliable fire-and-forget delivery system in Bridge.

### Task 10: Documentation, packaging, and release verification

**Files:** update `README.md`; create `docs/{architecture.md,voice-pipeline.md,voice-latency.md}`; update `docs/contracts/core.md`, `Dockerfile`, and release ownership test only where supported by evidence. Treat `ops/Caddyfile`, `ops/vox-bridge.service`, and deleted workflows as migration candidates, not automatically disposable files.

- [ ] Write the maintained Bridge pipeline guide from the reconciled ownership table, linking Core responsibilities and marking proposed features separately from implemented behavior. Reference both supplied documents as source material; replace machine-specific `file://` links.
- [ ] Document each environment variable by owner, enabled-channel requirements, secret status, default, and startup validation. Explain MP3 conversion and optional speaker-model behavior accurately.
- [ ] Consolidate authoritative production routing in `vox-deploy` as a separately reviewed change. Bridge should not own mobile/desktop `/v1` routing, global Redis admin routing, or deployment credentials. Verify consumers before retiring legacy ops files.
- [ ] Resolve the missing-workflow test against the chosen release path: either retain real publication workflows or replace obsolete string checks with tests of the supported release contract. Do not assert the full suite passes while leaving a known broken check.
- [ ] Verify Docker/native packaging against actual deployment. Keep the necessary ONNX runtime libraries, pin model artifacts/checksums if bundled, and run a separate model-backed smoke test; the existing conditional unit test is insufficient evidence. Do not change provider/model packaging merely as part of a move.
- [ ] Run the final local gates once behavior is stable:

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --release --locked
```

- [ ] Record staging verification separately: known/unknown greeting, Redis unavailable, numeric fragments and corrections, lookup filler, true interruption, slow Core, provider disconnect, WhatsApp request, and supported outbound callback. Use an authorized test caller/account; local test success does not authorize or prove production release.
- [ ] Roll out structural changes before latency changes. Keep the last known working artifact and compatible Core protocol available for rollback. Drain calls during restart; do not combine a provider swap with this release.
- [ ] Produce a completion report separating source changes, local tests, packaging, source publication, deployment, and live-call evidence. Commit or push only when separately requested.

## Completion criteria

- Bridge contains only communication-provider adapters, the Core client, and ephemeral voice/media state.
- UI CRUD, device data ingestion, desktop delegation, agent tools, databases, and commerce integrations remain Core concerns.
- Existing tests survive moves; added replay and contract tests cover the changed boundaries.
- No raw audio or private transcript fixtures are introduced, and normal logs avoid transcript/credential exposure.
- Module names reflect responsibilities; no unused agent registry, orphan Jev client, or unconsumed broadcast API remains.
- Pending lookup acknowledgment, stale-output rejection, and numeric-fragment assembly have explicit tested behavior.
- Every documented latency figure is labeled measured, configured, or aspirational, and configuration defaults match source.
- Cross-repo outbound/WhatsApp durability gaps are implemented with contract tests or explicitly marked unavailable; they are never hidden by a successful Bridge build.

## Suggested review boundaries

1. Baseline/contracts and bootstrap extraction (Tasks 1–2).
2. Core client and channel/provider organization (Tasks 3–4).
3. Voice module extraction and replay baseline (Tasks 5–6).
4. Playback invalidation and lifecycle fixes (Task 7).
5. Metrics and filler policy; separate optional chunker experiment (Task 8).
6. Coordinated channel-action contracts (Task 9, independent dependency track).
7. Documentation, packaging, and release ownership (Task 10).

These are proposed implementation/review batches, not commits created by this planning task.
