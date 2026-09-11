# Configurable Voice Pipeline Design

## Goal

Complete the inbound voice loop from Twilio audio through AssemblyAI speech recognition and the Gemini agent to Sarvam text-to-speech, while keeping telephony, STT, agent, and TTS providers independently replaceable.

## Initial Provider Profile

The first working profile uses:

- Twilio bidirectional Media Streams for inbound and outbound call audio
- AssemblyAI Universal Streaming for speech-to-text
- Gemini 3.5 Flash Lite and the existing agent tools for response generation
- Sarvam `bulbul:v3` for text-to-speech
- Sarvam language `en-IN`, speaker `shubh`, codec `mulaw`, and sample rate `8000`

Provider names, model names, language, speaker, and audio settings are startup configuration. Configuration is validated before the server starts.

## Scope

This change includes the complete single-call conversational loop, caller barge-in, provider timeouts, session cleanup, configuration documentation, and automated tests that do not call paid external APIs.

This change does not implement Exotel, additional STT or TTS vendors, dynamic mid-call provider migration, persistent conversation history, outbound call initiation, or a separate control agent. The boundaries needed for those additions are included.

No comments will be added to source code.

## Architecture

The system has two boundary layers:

1. A telephony adapter translates provider-specific call events into normalized call audio events and translates normalized outbound audio commands into provider messages.
2. A voice session coordinates selected STT, agent, and TTS implementations through provider interfaces.

A `VoiceProfile` describes the STT, agent, and TTS provider and model selections for a call. A `VoiceProfileResolver` supplies that profile. The initial resolver reads validated startup configuration. A future agent-backed resolver can return a profile per call without changing the session coordinator.

Inbound telephony is selected by the route that receives the call. A call already connected through Twilio cannot migrate to Exotel. A future outbound-call coordinator can use a telephony selection from the same policy layer before placing a call.

## Components

### Configuration

`VoiceConfig` reads and validates the selected provider identifiers and provider-specific settings. It rejects unsupported providers, missing credentials, invalid sample rates, and incompatible Sarvam model and speaker combinations before accepting calls.

The initial configuration keys are:

- `VOX_STT_PROVIDER`, default `assemblyai`
- `VOX_AGENT_PROVIDER`, default `gemini`
- `VOX_TTS_PROVIDER`, default `sarvam`
- `ASSEMBLYAI_API_KEY`
- `ASSEMBLYAI_SPEECH_MODEL`, default `universal-3-5-pro`
- `GEMINI_API_KEY`
- `GEMINI_MODEL`, default `gemini-3.5-flash-lite`
- `SARVAM_API_KEY`
- `SARVAM_TTS_MODEL`, default `bulbul:v3`
- `SARVAM_LANGUAGE_CODE`, default `en-IN`
- `SARVAM_SPEAKER`, default `shubh`
- `SARVAM_TTS_PACE`, default `1.0`

Twilio continues to use `TWILIO_AUTH_TOKEN`. Sarvam output is fixed to μ-law at 8000 Hz for the Twilio adapter in this profile.

### Provider Registry

The provider registry constructs the STT, agent, and TTS implementation selected by a `VoiceProfile`. Unsupported selections fail explicitly. Provider creation and selection are separate from call orchestration.

### AssemblyAI STT

The AssemblyAI implementation opens one authenticated v3 streaming WebSocket per call with μ-law encoding and an 8000 Hz sample rate. It forwards raw decoded Twilio audio as binary frames. Only non-empty `Turn` events with `end_of_turn: true` become user turns. Speech-start events are also exposed for barge-in. The session sends AssemblyAI's termination message during orderly shutdown.

### Gemini Agent

The Gemini implementation wraps the existing agent and tools behind the agent provider interface. Each finalized transcript produces one text response. Calls for a single voice session are serialized so two user turns cannot concurrently mutate playback state.

### Sarvam TTS

The Sarvam implementation sends each Gemini response to the HTTP streaming endpoint. It requests `bulbul:v3`, `en-IN`, `shubh`, μ-law output, and an 8000 Hz sample rate. Successful binary response chunks are forwarded without transcoding. Non-success responses become sanitized provider errors and response bodies are not logged.

### Twilio Adapter

The Twilio webhook keeps its existing form parsing and signature validation. The WebSocket upgrade also validates Twilio's signature. After the `start` event, the adapter verifies that `CallSid` belongs to a call accepted by the signed webhook.

Inbound `media` payloads are base64-decoded before being sent to STT. Outbound μ-law chunks are base64-encoded as Twilio `media` messages. The adapter sends a `mark` after a complete response and sends `clear` when playback is interrupted.

## Call Lifecycle

1. Twilio invokes the signed voice webhook.
2. The webhook stores call state and returns TwiML connecting a bidirectional Media Stream.
3. The WebSocket handshake is validated.
4. The Twilio `start` event binds the stream to accepted call state.
5. Incoming media is decoded and forwarded to AssemblyAI.
6. AssemblyAI emits a finalized transcript.
7. Gemini produces a text response using the existing tools when needed.
8. Sarvam streams μ-law/8000 audio.
9. The Twilio adapter sends audio chunks followed by a playback mark.
10. A speech-start event during playback cancels the active TTS task and clears Twilio's buffered audio.
11. A stop event, socket disconnect, or unrecoverable audio error terminates provider sessions and removes call state.

## Concurrency and Backpressure

Each call owns a bounded audio channel between the telephony adapter and STT. If the consumer cannot keep up and the buffer fills, the session ends rather than growing memory without limit. Finalized turns are processed in order. Only one Gemini and Sarvam response pipeline runs at a time per call.

Playback has an explicit cancellation handle. Caller speech cancels current synthesis and clears already buffered Twilio audio before the next finalized turn is processed.

## Failure Handling

Malformed provider messages are logged with call and stream identifiers but without audio, transcripts, credentials, signatures, or remote response bodies. Invalid base64 media and an unexpected stream identifier terminate the affected session.

Connection establishment, agent response generation, and TTS response headers have bounded timeouts. A failed user turn is discarded if the audio connection remains usable. An STT or telephony failure ends the session. Shutdown attempts to terminate AssemblyAI and remove call state even when another provider fails.

## Testing

Automated tests exercise real parsing, state transitions, request construction, and serialization while replacing only external network boundaries.

Coverage includes:

- Valid and invalid provider configuration
- Twilio signature validation for the current webhook and stream routes
- Twilio inbound event parsing and outbound media, mark, and clear messages
- AssemblyAI binary audio forwarding and finalized-turn filtering
- Sarvam request fields and streamed μ-law chunk forwarding
- The complete normalized audio-to-transcript-to-response-to-audio orchestration
- Barge-in cancellation and Twilio buffer clearing
- Cleanup after stop, disconnect, malformed input, and provider failure
- Existing health, Gemini tool, and webhook behavior

Verification consists of formatting checks, the full test suite, strict linting, a release build, and a clean diff check. Live paid calls are separate deployment verification and are not implied by local success.

## Deployment Contract

Deployment requires the AssemblyAI and Sarvam credentials in the protected service environment alongside the existing Twilio, Gemini, Exa, and Google Maps configuration. Secrets are never printed during installation or verification.

A service restart only reloads the currently installed binary and environment. End-to-end completion requires deploying the newly built binary, waiting for startup, checking service activity and health, validating controlled webhook cases, and placing a real call with the configured providers.
