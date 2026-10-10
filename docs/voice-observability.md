# Voice latency logs

Core and Bridge emit structured timing events at INFO. Set `LOG_FORMAT=json` for local runs; Railway uses JSON automatically. No new prompt, transcript, user-name, phone-number, tool-argument, tool-result, or audio-content fields are logged. Existing opt-in Langfuse content tracing is independent of these timings.

Deploy both updated services to collect this instrumentation. Export their logs over the same call window, including the call's start and close. Export one JSON object per line, either the original event or a Railway object whose `message` contains it. Then run:

```sh
python3 scripts/analyze-voice-logs.py bridge.jsonl core.jsonl
python3 scripts/analyze-voice-logs.py bridge.jsonl core.jsonl --json > voice-analysis.json
```

The script reports call/turn/revision outcomes, completed-turn p50/p95 with sample counts, slow turns, individual tool executions, and missing correlations. JSON output includes every turn, allowing comparison across early and late turns. It joins transport generations to turn reports even when exports arrive out of order. Missing observations stay unknown; cancelled, failed, degraded, and silent turns are excluded from completed-only percentiles and remain in the outcome counts. Keep those counts alongside latency percentiles to avoid making interruptions look like improvements.

## Correlation

`conversation_id` is the external call/conversation identifier shared by Core and Bridge. `turn_id` and `revision` distinguish an utterance and its corrections. Bridge adds playback `generation` and sequential `turn`; Twilio adds `call_sid` and `stream_sid`. Greeting requests have a turn ID too. Native voice adds `session_id`. Common context is present in JSON `span`/`spans` as well as explicit event fields. Tool executions have a separate `execution_id`; repeated calls to the same tool remain distinguishable.

## Greeting

| Event | What it measures |
| --- | --- |
| `VOICE_WEBHOOK_ACCEPTED` | Inbound webhook validation duration. |
| `VOICE_TWILIO_START` | Webhook acceptance to media-stream start, and socket connection to start. Webhook timing is unknown after outbound state recovery or on another instance. |
| `VOICE_GREETING_DISPATCHED` | Session entry to greeting dispatch. |
| `VOICE_STT_CONNECTED` | Initial STT connection duration. Greeting generation starts before this await; STT setup can still delay the socket/session input path. |
| `CORE_CACHED_GREETING_METRICS` | Name lookup and synchronous conversation resolution before the cached greeting is returned. `name_known` describes available metadata, not a Redis cache hit. |
| `CORE_NAME_LOOKUP` | Actual Redis source or Postgres fallback, cache miss/empty/error/disabled, and query timing. No name value is included. |
| `VOICE_TRANSPORT_FIRST_AUDIO` | First successful WebSocket audio write per generation and audio kind. For greeting answers, `turn_to_first_ws_audio_ms` starts at greeting dispatch. |
| `VOICE_NATIVE_FIRST_AUDIO_SENT` | Native turn receipt to first WebSocket audio, plus session entry to that audio. |

Core's response headers expose context resolution and service preparation. `CORE_AGENT_AND_OPENING_PREPARATION` includes agent selection and any pending opening wait even for cached greetings; `CORE_NAME_CACHE_REFRESH` measures a named Postgres lookup's cache refresh. Bridge records those values plus client preparation and send-to-headers duration. `transport_and_client_overhead_ms` is a residual estimate, not a measurement of pure network latency. Webhook-to-start can include Twilio orchestration, handshake, and transit; it is not attributed to a specific provider.

## Turns

`VOICE_TURN_LATENCY_REPORT` is emitted on completion, error, or task cancellation. It includes:

- `speech_to_final_ms`: last input frame with detectable mu-law energy to the latest final transcript. This is explicitly a **passive energy estimate**, affected by noise, echo, buffering, and detector threshold; it is not exact speech-end detection or pure STT provider latency. Silence packets no longer extend the endpoint. The detector never changes turn behavior.
- `dispatch_wait_ms`: first final transcript to dispatch, including transcript accumulation and settling. `final_to_dispatch_ms` starts at the latest final transcript.
- `filler_decision_ms`: acknowledgement choice before the Core request. Normal task turns can wait up to 250 ms for Jev before falling back.
- `core_headers_ms`, `core_first_text_ms`, `core_stream_ms`: Bridge-observed Core request phases, including Core work and transport. These are not labelled model TTFT.
- `text_to_sentence_ms`: first Core text to first speakable sentence. Sentence buffering can delay audible responses even when tokens arrive quickly.
- `first_tts_ttfb_ms`: first answer TTS request to first audio chunk, before output enqueueing.
- `transcript_to_answer_enqueue_ms` and `speech_end_to_answer_enqueue_ms`: answer audio enqueueing milestones. Filler has its own metric and apology audio has its own kind.
- `response_total_ms`, `last_stage`, text/chunk/sentence counts, and TTS error count: generation/drain duration and diagnostic state for incomplete turns.

`VOICE_TTS_SENTENCE_STARTED` reports the sentence queue wait. `VOICE_TTS_SENTENCE` reports provider headers, first chunk, synthesis/drain time, observed chunk gaps, bytes, frames, and outcome, including cancellation. Gap measurements can include local consumer backpressure; they do not prove a provider stall.

`VOICE_TRANSPORT_FIRST_AUDIO` reports actual successful WebSocket writes. For answer audio, `turn_to_first_ws_audio_ms` uses the first final transcript as its origin (greeting dispatch for greetings). `queue_ms` includes waiting to enqueue and waiting in the output queue; `write_ms` includes serialization and the WebSocket send. `VOICE_TRANSPORT_SUMMARY` reports maximum queue/write/frame gaps and audio totals. `VOICE_TRANSPORT_MARK_ACK` records mark round-trip and whether the mark was invalidated by clear. A cleared mark is never treated as played audio. Neither a successful socket write nor an ordinary playback mark proves the caller heard it: client decoding, network transit, and device playback are outside these measurements.

`VOICE_SESSION_SUMMARY` aggregates audio frames/bytes, partial/final transcripts, backchannels, interruptions, STT errors and reconnects. STT send waits above 100 ms get a warning. Native voice has `VOICE_STT`, `VOICE_TURN_REPLY`, and `VOICE_NATIVE_TURN_LIFECYCLE`: Core and actual WebSocket timings are separate, and failed/empty/cancelled turns remain visible even when the task exits before a normal reply summary. Native timing starts when the server accepts the turn, not at the client's last spoken sample.

## Core work

- `CORE_STREAM_FIRST_TEXT` includes host-context resolution, service preparation and total first-text timing; `CORE_STREAM_LIFECYCLE` records complete, failed, or cancelled SSE delivery.
- `CORE_TURN_PREPARATION` includes agent selection, opening initialization wait, conversation resolution, and parallel history/name loading (`history_and_name_ms`). `CORE_CONTEXT_PROJECTION` reports memory projection time and bytes.
- `CORE_AGENT_PREPARATION` reports model/agent selection and prompt history/context size before streaming begins.
- `CORE_MODEL_FIRST_TEXT` starts immediately before Rig's streaming request and excludes service/agent preparation. It includes provider client/request setup and time until the stream is polled; it is not an isolated network measurement. Cached inbound greetings bypass the model entirely.
- `CORE_MODEL_ROUND_FINISHED` reports each completion round, provider-reported token usage/cache/reasoning counts and finish reason. Missing usage is unknown, not zero. Slow multi-round tool workflows remain distinguishable from a simple answer.
- `CORE_TOOL_STARTED` / `CORE_TOOL_FINISHED` wrap actual top-level conversational tool bodies with execution ID, duration and completed/failed/cancelled outcome. No arguments/results are printed. A completed tool body can return a business-level rejection; execution completion does not prove an external action succeeded. Nested library workflows retain their existing tracing.
- `CORE_MODEL_FINISHED` reports model-stream duration, first-text timing and complete/failed/cancelled outcome, including partial replies.

For long calls compare early/late turns with similar tasks: history/context bytes, input tokens, number of model rounds, individual tool durations, first-answer socket latency, and TTS/output gaps. Stage durations overlap (model output and TTS run concurrently, history/name load concurrently, tools may run concurrently); do not sum every stage into a total. Use each service's monotonic durations, and IDs to join services, rather than subtracting wall-clock timestamps across hosts.

These changes add observability, not a demonstrated production latency improvement. A real call after deployment is needed to identify the actual greeting or turn bottleneck.
