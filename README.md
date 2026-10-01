# Vox bridge

## Voice calls

Inbound Twilio calls use a bidirectional Media Stream through a configurable
voice pipeline:

`Twilio -> AssemblyAI -> Vox Core -> ElevenLabs -> Twilio`

The Twilio adapter accepts μ-law audio at 8000 Hz. AssemblyAI transcribes the
incoming stream, finalized turns are sent to Vox Core, and
the selected TTS provider streams the response back as μ-law audio at 8000 Hz without
transcoding. Caller speech interrupts active playback.

Every variable the bridge reads, with which are required and their defaults, is
documented in `../vox-edge/.env.example`. Set the required ones in the local `.env`
and in the deployed service environment: `VOX_AUTH_TOKEN`, `TWILIO_ACCOUNT_SID`,
`TWILIO_AUTH_TOKEN`, `TWILIO_FROM_NUMBER`, `ASSEMBLYAI_API_KEY`,
`VOX_HOST_CREDENTIAL_ID`, `VOX_HOST_AUDIENCE`, `VOX_HOST_SECRET` and `ELEVENLABS_API_KEY`.

Speech uses AssemblyAI (`universal-3-5-pro`) for transcription and ElevenLabs
(`eleven_v4_turbo`) for text-to-speech. The models are fixed in code; only the keys
and optional voice IDs are configured (see the env reference above). The provider
stages are still registered independently, so an additional provider can be added in
`voice/registry.rs`.

Configure Twilio Voice to send incoming call webhooks to:

`POST https://api.voxagent.in/bridge/twilio/voice`

The returned TwiML connects Twilio to:

`wss://api.voxagent.in/bridge/twilio/voice/stream`

Both requests validate `X-Twilio-Signature` before a call session is accepted.

## Core integration

Vox Bridge no longer owns Gemini prompts or agent tools. It sends finalized
Twilio and WhatsApp text turns to Vox Core with their channel identity and
provider conversation identifier. Gemini, Exa, Google Maps, persistent memory,
and scheduling credentials belong to the Core service.

Bridge is a registered Core host app. Register it once with Core, store the
returned credential ID, audience, and secret in deployment secret storage, and
configure the three `VOX_CORE_HOST_*` variables above. Bridge creates a fresh
timestamped, nonce-bound host assertion for every unary, streaming, speculation,
and completion call; it never uses the Core service token for conversations.
Rotate the Core host credential before replacing the Bridge secret, then revoke
the old credential after the rollout. Core resolves channel principals as
`twilio:<normalized-e164>` and `whatsapp:<normalized-e164>`; the same phone
number is intentionally not linked across channels without a later explicit
identity-linking flow.

Partial voice transcripts request bounded capability metadata warmup from Core.
Core discards the results; final turns use normal governed tool discovery.
Conversation streams contain text, error and terminal frames. No speculative
account data or lookup-status event is injected into the response.

Run `cargo test --locked`, strict Clippy, and a release build to verify locally.
Tests use local network boundaries and do not call paid provider APIs. Local
verification does not prove the binary is deployed or that a real phone call
works with the deployed credentials.

## Deploy on Railway

Deploy this repo as one service (`vox-bridge`) using the root `Dockerfile` with an empty root directory. It binds `0.0.0.0:$PORT`.

Variables:

- `VOX_CORE_URL`: `http://vox-core-api.railway.internal:<core PORT>`
- `VOX_AUTH_TOKEN`: the same value as on `vox-core-api` and `vox-core-worker`
- `TWILIO_ACCOUNT_SID`, `TWILIO_AUTH_TOKEN`, `TWILIO_FROM_NUMBER`
- `ASSEMBLYAI_API_KEY`, `ELEVENLABS_API_KEY`

Only the edge (`vox-edge`) has a public domain. It routes `/bridge/*` here, so Twilio and WhatsApp webhooks point at `https://api.voxagent.in/bridge/...`. The service also exposes `/health/live`, `/health/ready` and `/internal/v1/**`, none of which the edge forwards. See `vox-edge/ROUTING.md`.
