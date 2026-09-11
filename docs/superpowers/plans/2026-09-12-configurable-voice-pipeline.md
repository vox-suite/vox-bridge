# Configurable Voice Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a working Twilio to AssemblyAI to Gemini to Sarvam voice loop with independently selectable STT, agent, and TTS providers.

**Architecture:** Provider-specific network clients implement small async interfaces consumed by a provider-neutral `VoiceSession`. The Twilio adapter translates WebSocket events into normalized audio input and converts normalized output commands back to Twilio messages. A validated `VoiceProfile` is resolved before a session begins, allowing a future policy agent to select implementations without changing orchestration.

**Tech Stack:** Rust 2024, Axum WebSockets, Tokio, tokio-tungstenite, reqwest streaming, Rig Gemini agents, AssemblyAI v3 Streaming, Sarvam HTTP Streaming TTS

**Spec:** `docs/superpowers/specs/2026-09-12-configurable-voice-pipeline-design.md`

## Global Constraints

- Keep Twilio form and WebSocket signature validation ahead of session creation.
- Use AssemblyAI `universal-3-5-pro` with `pcm_mulaw` at 8000 Hz by default.
- Use Gemini `gemini-3.5-flash-lite` with the existing agent tools and ten-turn limit by default.
- Use Sarvam `bulbul:v3`, `en-IN`, `shubh`, `mulaw`, and 8000 Hz by default.
- Never log credentials, signatures, remote response bodies, audio payloads, or transcripts.
- Do not add comments to source code.
- Preserve unrelated existing worktree changes and retain the current Exa and Google Maps agent behavior.
- Automated tests must not call paid external APIs.

---

### Task 1: Voice configuration and provider contracts

**Files:**
- Create: `src/voice/mod.rs`
- Create: `src/voice/config.rs`
- Create: `src/voice/provider.rs`
- Modify: `src/main.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Test: `src/voice/config.rs`

**Interfaces:**
- Produces: `VoiceConfig::from_values`, `VoiceConfig::from_env`, `VoiceProfile`, `SttProvider`, `SttSession`, `AgentProvider`, `TtsProvider`, `SttEvent`, `AudioStream`, and `VoiceError`.
- Consumes: no earlier task interfaces.

- [ ] **Step 1: Write failing configuration tests**

Add table-driven tests that pass literal key-value maps to `VoiceConfig::from_values` and verify the exact defaults. Add separate tests that reject `deepgram`, `openai`, and `elevenlabs` as unsupported selections, reject absent required credentials, reject a Sarvam pace outside `0.5..=2.0`, and reject a Sarvam speaker incompatible with `bulbul:v3`.

The successful expectation must be:

```rust
assert_eq!(config.profile.stt.provider, "assemblyai");
assert_eq!(config.profile.stt.model, "universal-3-5-pro");
assert_eq!(config.profile.agent.provider, "gemini");
assert_eq!(config.profile.agent.model, "gemini-3.5-flash-lite");
assert_eq!(config.profile.tts.provider, "sarvam");
assert_eq!(config.profile.tts.model, "bulbul:v3");
assert_eq!(config.profile.tts.language_code, "en-IN");
assert_eq!(config.profile.tts.speaker, "shubh");
```

- [ ] **Step 2: Run the configuration tests and verify the missing module failure**

Run: `cargo test voice::config::tests --locked`

Expected: compilation fails because `voice::config` and `VoiceConfig` do not exist.

- [ ] **Step 3: Add dependencies and provider-neutral types**

Add `async-trait`, `bytes`, and `thiserror`. Enable reqwest's `stream` feature. Retain the current uncommitted `tokio-tungstenite` addition.

Define these contracts without source comments:

```rust
pub type AudioStream = Pin<Box<dyn Stream<Item = Result<Bytes, VoiceError>> + Send>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttEvent {
    SpeechStarted,
    FinalTranscript(String),
}

#[async_trait]
pub trait SttSession: Send {
    async fn send_audio(&mut self, audio: Bytes) -> Result<(), VoiceError>;
    async fn next_event(&mut self) -> Result<Option<SttEvent>, VoiceError>;
    async fn finish(&mut self) -> Result<(), VoiceError>;
}

#[async_trait]
pub trait SttProvider: Send + Sync {
    async fn connect(&self) -> Result<Box<dyn SttSession>, VoiceError>;
}

#[async_trait]
pub trait AgentProvider: Send + Sync {
    async fn respond(&self, transcript: &str) -> Result<String, VoiceError>;
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<AudioStream, VoiceError>;
}
```

Use `VoiceError` variants `Configuration(String)`, `Provider { provider: &'static str, message: String }`, `Protocol(String)`, and `Timeout(&'static str)`.

- [ ] **Step 4: Implement validated configuration**

Implement `from_values` using a string lookup closure so tests never mutate process-wide environment variables. Implement `from_env` as a thin adapter that collects only the documented configuration keys and calls `from_values`. Keep credentials in private fields on `VoiceConfig` and do not derive `Debug` for the credential container.

- [ ] **Step 5: Run focused tests and compiler checks**

Run: `cargo test voice::config::tests --locked && cargo check --locked`

Expected: the configuration tests pass and the crate compiles. The already-known stale TwiML assertion remains isolated until Task 6.

- [ ] **Step 6: Commit the configuration boundary**

Run: `git add Cargo.toml Cargo.lock src/main.rs src/voice && git commit -m "feat: add configurable voice provider contracts"`

### Task 2: AssemblyAI streaming STT provider

**Files:**
- Modify: `src/agents/mod.rs`
- Modify: `src/agents/stt/mod.rs`
- Modify: `src/agents/stt/assembly_stt.rs`
- Test: `src/agents/stt/assembly_stt.rs`

**Interfaces:**
- Consumes: `SttProvider`, `SttSession`, `SttEvent`, and `VoiceError` from Task 1.
- Produces: `AssemblyAiStt::new(api_key, model)` and its `SttProvider` implementation.

- [ ] **Step 1: Replace the unfinished module body with failing protocol tests**

Preserve the local file as the implementation location. Add tests proving that a literal final event:

```json
{"type":"Turn","turn_order":1,"end_of_turn":true,"transcript":"Book a table"}
```

becomes `Some(SttEvent::FinalTranscript("Book a table".into()))`, while partial and whitespace-only turns become `None`. Add a test proving `{"type":"SpeechStarted","timestamp":40}` becomes `Some(SttEvent::SpeechStarted)`. Add a binary-frame test asserting that bytes `[0xff, 0x7f]` are sent unchanged.

- [ ] **Step 2: Run the AssemblyAI tests and verify the expected failures**

Run: `cargo test agents::stt::assembly_stt::tests --locked`

Expected: tests fail because the event conversion and working session implementation are absent.

- [ ] **Step 3: Implement the AssemblyAI protocol**

Build the endpoint with `speech_model`, `encoding=pcm_mulaw`, and `sample_rate=8000`. Authenticate with the `Authorization` header without a bearer prefix. Split the provider WebSocket into a sink and stream owned by `AssemblyAiSession`.

`send_audio` must send `TungsteniteMessage::Binary(audio)`. `next_event` must ignore unknown provider events and partial turns, map speech-start and final-turn events, and return a sanitized provider error for malformed frames. `finish` must send this exact JSON and close the socket:

```json
{"type":"Terminate"}
```

- [ ] **Step 4: Run focused tests and compiler checks**

Run: `cargo test agents::stt::assembly_stt::tests --locked && cargo check --locked`

Expected: focused tests pass and the crate compiles without the spelling and missing-import errors in the unfinished local file.

- [ ] **Step 5: Commit the STT provider**

Run: `git add src/agents/mod.rs src/agents/stt && git commit -m "feat: stream call audio through AssemblyAI"`

### Task 3: Sarvam streaming TTS provider

**Files:**
- Create: `src/agents/tts/mod.rs`
- Create: `src/agents/tts/sarvam_tts.rs`
- Modify: `src/agents/mod.rs`
- Test: `src/agents/tts/sarvam_tts.rs`

**Interfaces:**
- Consumes: `TtsProvider`, `AudioStream`, and `VoiceError` from Task 1.
- Produces: `SarvamTts::new(http, api_key, endpoint, settings)` and its `TtsProvider` implementation.

- [ ] **Step 1: Write failing local-server tests**

Start an Axum listener on `127.0.0.1:0`. Capture the request headers and JSON body, then stream two literal byte chunks. Assert that `synthesize("Hello")` returns both chunks unchanged and that the captured request contains:

```json
{
  "text":"Hello",
  "language_code":"en-IN",
  "speaker":"shubh",
  "model":"bulbul:v3",
  "pace":1.0,
  "speech_sample_rate":8000,
  "output_audio_codec":"mulaw"
}
```

Add a separate test for a non-success response. Assert only the sanitized error category, never the remote body.

- [ ] **Step 2: Run the Sarvam tests and verify the missing implementation failure**

Run: `cargo test agents::tts::sarvam_tts::tests --locked`

Expected: compilation fails because `SarvamTts` does not exist.

- [ ] **Step 3: Implement HTTP streaming synthesis**

POST to `/text-to-speech/stream` with `api-subscription-key` and JSON content type. Reject blank text locally. Require a success status before exposing `response.bytes_stream()` as `AudioStream`. Convert network failures into `VoiceError::Provider` without including response bodies or the API key.

- [ ] **Step 4: Run focused tests and compiler checks**

Run: `cargo test agents::tts::sarvam_tts::tests --locked && cargo check --locked`

Expected: Sarvam tests pass and the crate compiles. The pre-existing stale TwiML assertion remains isolated until Task 6.

- [ ] **Step 5: Commit the TTS provider**

Run: `git add src/agents/mod.rs src/agents/tts && git commit -m "feat: add Sarvam streaming speech output"`

### Task 4: Gemini agent provider and runtime registry

**Files:**
- Modify: `src/agents/agent.rs`
- Create: `src/voice/registry.rs`
- Modify: `src/voice/mod.rs`
- Test: `src/voice/registry.rs`

**Interfaces:**
- Consumes: `AgentProvider`, `SttProvider`, `TtsProvider`, `VoiceConfig`, `VoiceProfile`, `AssemblyAiStt`, and `SarvamTts`.
- Produces: `GeminiAgent`, `ProviderRegistry::from_config`, `VoiceRuntime::from_config`, and `VoiceProfileResolver`.

- [ ] **Step 1: Write failing registry tests**

Construct validated configurations with literal credentials and verify that `VoiceRuntime::from_config` resolves the default profile. Add table cases confirming unsupported selections return `VoiceError::Configuration` and never silently fall back.

- [ ] **Step 2: Run registry tests and verify the missing type failure**

Run: `cargo test voice::registry::tests --locked`

Expected: compilation fails because `VoiceRuntime` and `GeminiAgent` do not exist.

- [ ] **Step 3: Wrap the existing Gemini implementation**

Move the existing Rig client construction behind `GeminiAgent::new`. Keep Exa, Google Maps, the safety preamble, and `.default_max_turns(10)`. Read keys and model names from `VoiceConfig` rather than from the environment inside each request. Implement `AgentProvider::respond` and return sanitized `VoiceError::Provider` values.

- [ ] **Step 4: Build the runtime registry**

Define:

```rust
pub trait VoiceProfileResolver: Send + Sync {
    fn resolve(&self) -> VoiceProfile;
}

pub struct ProviderRegistry {
    stt: HashMap<String, Arc<dyn SttProvider>>,
    agents: HashMap<String, Arc<dyn AgentProvider>>,
    tts: HashMap<String, Arc<dyn TtsProvider>>,
}

pub struct VoiceRuntime {
    pub providers: Arc<ProviderRegistry>,
    pub resolver: Arc<dyn VoiceProfileResolver>,
}
```

Create the default AssemblyAI, Gemini, and Sarvam implementations exactly once at startup and insert them under their provider identifiers. The static resolver clones the validated profile. `ProviderRegistry::providers_for(&VoiceProfile)` resolves all three implementations or returns an explicit configuration error. This keeps a future per-call resolver meaningful when more implementations are registered.

- [ ] **Step 5: Run registry and existing agent tests**

Run: `cargo test voice::registry::tests --locked && cargo test agents:: --locked`

Expected: all selected tests pass; the live Gemini test remains ignored.

- [ ] **Step 6: Commit the runtime registry**

Run: `git add src/agents/agent.rs src/voice && git commit -m "refactor: select voice providers through runtime profile"`

### Task 5: Provider-neutral voice session orchestration

**Files:**
- Create: `src/voice/session.rs`
- Modify: `src/voice/mod.rs`
- Test: `src/voice/session.rs`

**Interfaces:**
- Consumes: `VoiceRuntime`, `SttEvent`, `AudioStream`, and all provider traits.
- Produces: `CallEvent`, `CallCommand`, and `run_voice_session(runtime, input, output)`.

- [ ] **Step 1: Write failing end-to-end orchestration tests**

Use in-memory fake implementations of the provider traits and bounded Tokio channels. Feed `CallEvent::Audio(Bytes::from_static(&[0xff, 0x7f]))`, then a final transcript of `hello`. Verify the fake agent receives exactly `hello`, the fake TTS receives exactly `Hi there`, and output contains the literal audio chunks followed by a mark:

```rust
assert_eq!(commands, vec![
    CallCommand::Media(Bytes::from_static(&[1, 2])),
    CallCommand::Media(Bytes::from_static(&[3, 4])),
    CallCommand::Mark("response-1".into()),
]);
```

Add tests for partial transcript suppression, serialized final turns, TTS failure without session leakage, and stop cleanup.

- [ ] **Step 2: Run orchestration tests and verify the missing session failure**

Run: `cargo test voice::session::tests --locked`

Expected: compilation fails because `run_voice_session`, `CallEvent`, and `CallCommand` do not exist.

- [ ] **Step 3: Implement the basic session loop**

Define:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEvent {
    Audio(Bytes),
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallCommand {
    Media(Bytes),
    Mark(String),
    Clear,
}
```

Open one STT session per call. Forward audio to STT and process STT events concurrently. Serialize final transcripts through agent and TTS. Stream each successful TTS chunk as `CallCommand::Media`, then emit a monotonically numbered response mark.

- [ ] **Step 4: Add failing barge-in test**

Use a TTS fake whose stream remains pending after its first chunk. Emit `SttEvent::SpeechStarted` and assert the next command is `CallCommand::Clear`, the pending synthesis future is dropped, and the session accepts the next finalized transcript.

- [ ] **Step 5: Run the barge-in test and verify it fails before cancellation exists**

Run: `cargo test voice::session::tests::speech_started_cancels_playback_and_clears_twilio --locked`

Expected: failure because the pending synthesis remains active or no `Clear` command is emitted.

- [ ] **Step 6: Implement playback cancellation and shutdown**

Own active response generation in a Tokio task. Abort it on `SpeechStarted`, emit one `Clear`, and join or discard the cancelled task before accepting the next response. On stop or input closure, call `SttSession::finish` with a timeout and close the output channel.

- [ ] **Step 7: Run focused tests and compiler checks**

Run: `cargo test voice::session::tests --locked && cargo check --locked`

Expected: orchestration tests pass and the crate compiles. The stale TwiML assertion remains isolated until Task 6.

- [ ] **Step 8: Commit orchestration**

Run: `git add src/voice && git commit -m "feat: orchestrate streaming voice sessions"`

### Task 6: Secure Twilio Media Streams adapter

**Files:**
- Modify: `src/routes/twilio/twilio_post.rs`
- Modify: `src/routes/twilio/twilio_socket.rs`
- Modify: `src/main.rs`
- Test: `src/routes/twilio/twilio_post.rs`
- Test: `src/routes/twilio/twilio_socket.rs`

**Interfaces:**
- Consumes: `VoiceRuntime`, `CallEvent`, `CallCommand`, and `run_voice_session`.
- Produces: a signed `/bridge/twilio/voice` webhook and authenticated `/bridge/twilio/voice/stream` WebSocket wired to the complete pipeline.

- [ ] **Step 1: Write failing Twilio protocol tests**

Update the stale TwiML test to require the current stream URL. Add exact serialization tests for:

```json
{"event":"media","streamSid":"MZ123","media":{"payload":"AQI="}}
```

```json
{"event":"mark","streamSid":"MZ123","mark":{"name":"response-1"}}
```

```json
{"event":"clear","streamSid":"MZ123"}
```

Add parser tests for connected, start, media, mark, and stop events. Add tests rejecting a missing or invalid WebSocket `X-Twilio-Signature` before upgrade and rejecting a `start.callSid` absent from accepted call state.

- [ ] **Step 2: Run Twilio tests and verify current failures**

Run: `cargo test routes::twilio:: --locked`

Expected: the stale URL test fails and the new outbound/authentication tests fail because those behaviors are not implemented.

- [ ] **Step 3: Correct routing and shared signature validation**

Register both routes with leading slashes. Keep the public webhook and stream URLs in one Twilio configuration value used by TwiML and signature validation. Reuse the constant-time HMAC comparison for form callbacks and the WebSocket GET handshake. Return an HTTP error without upgrading when the stream signature is absent or invalid.

- [ ] **Step 4: Wire normalized call events and commands**

After a valid start event, confirm the call exists, bind its `streamSid`, and start `run_voice_session` with bounded channels. Decode inbound base64 media to bytes and send `CallEvent::Audio`. Encode `CallCommand::Media` to base64 and serialize media, mark, and clear messages with the active stream identifier. Treat a full audio channel, mismatched stream identifiers, and malformed payloads as protocol errors.

- [ ] **Step 5: Implement cleanup**

On Twilio stop, send `CallEvent::Stop`. On either socket or voice-session completion, stop the other side, close the WebSocket, and remove the call from `AppState.twilio`. Correct call and stream identifiers in structured logs without logging protected content.

- [ ] **Step 6: Run Twilio and full tests**

Run: `cargo test routes::twilio:: --locked && cargo test --locked`

Expected: all non-ignored tests pass with no external API calls.

- [ ] **Step 7: Commit the Twilio adapter**

Run: `git add src/main.rs src/routes/twilio && git commit -m "feat: connect Twilio calls to voice pipeline"`

### Task 7: Operations documentation and final verification

**Files:**
- Modify: `README.md`
- Modify: `ops/vox-bridge.service` only if its existing environment loading does not already cover the new variables
- Test: full repository

**Interfaces:**
- Consumes: the complete implementation from Tasks 1 through 6.
- Produces: documented configuration and fresh verification evidence.

- [ ] **Step 1: Document the provider profile**

Add a concise voice section listing every configuration key, default model, required credential, Twilio webhook URL, Twilio Media Stream URL, Sarvam μ-law/8000 contract, local verification commands, and the boundary between local completion and live-call validation.

- [ ] **Step 2: Run formatting and inspect resulting changes**

Run: `cargo fmt --all -- --check`

If it reports differences, run `cargo fmt --all`, inspect the diff, then rerun the check.

- [ ] **Step 3: Run full automated verification**

Run: `cargo test --locked`

Expected: every non-ignored test passes and the live Gemini test is the only ignored test.

- [ ] **Step 4: Run strict lint and release build**

Run: `cargo clippy --locked --all-targets --all-features -- -D warnings && cargo build --release --locked`

Expected: both commands exit successfully with no warnings.

- [ ] **Step 5: Verify repository hygiene and constraints**

Run: `git diff --check && git status --short`

Inspect every changed source file to confirm no source comments were added, no credentials are present, the design requirements map to tests, and unrelated worktree changes were preserved. Inspect added source lines with `git diff --unified=0 | rg '^\+.*//'` and remove any matching source comments without changing URL strings.

- [ ] **Step 6: Commit documentation and any formatting-only adjustments**

Run: `git add README.md ops/vox-bridge.service Cargo.toml Cargo.lock src && git commit -m "docs: describe configurable voice integration"`

Omit unchanged paths from the staging command after inspecting `git status`.

- [ ] **Step 7: Report deployment boundary**

Report local test, lint, and build evidence separately from deployment. A live call is verified only after the new binary and protected credentials are installed, the service becomes active, `/health` returns `ok`, controlled signed webhook cases return the expected statuses and TwiML, and an actual phone call completes STT, agent, and TTS playback.
