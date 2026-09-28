# Vox bridge

## Voice calls

Inbound Twilio calls use a bidirectional Media Stream through a configurable
voice pipeline:

`Twilio -> AssemblyAI -> Vox Core -> (Sarvam | ElevenLabs) -> Twilio`

The Twilio adapter accepts μ-law audio at 8000 Hz. AssemblyAI transcribes the
incoming stream, finalized turns are sent to Vox Core, and
the selected TTS provider streams the response back as μ-law audio at 8000 Hz without
transcoding. Caller speech interrupts active playback.

Set the required credentials in the local `.env` and in the deployed service
environment:

```dotenv
TWILIO_AUTH_TOKEN=your-twilio-auth-token
ASSEMBLYAI_API_KEY=your-assemblyai-api-key
VOX_CORE_URL=http://core-api:3001
VOX_CORE_HOST_CREDENTIAL_ID=replace-with-core-issued-credential-id
VOX_CORE_HOST_AUDIENCE=vox-host:deployment:bridge
VOX_CORE_HOST_SECRET=replace-with-core-issued-host-secret
ELEVENLABS_API_KEY=your-elevenlabs-api-key
```

The default provider profile uses ElevenLabs TTS:

```dotenv
VOX_STT_PROVIDER=assemblyai
ASSEMBLYAI_SPEECH_MODEL=universal-3-5-pro
VOX_TTS_PROVIDER=elevenlabs
ELEVENLABS_MODEL_ID=eleven_flash_v2_5
ELEVENLABS_VOICE_ID=21m00Tcm4TlvDq8ikWAM
```

To use Sarvam for text-to-speech:

```dotenv
VOX_TTS_PROVIDER=sarvam
SARVAM_API_KEY=your-sarvam-api-key
```

Sarvam model, speaker, language, and pace are fixed in code (`bulbul:v3`, `shubh`, `en-IN`, `1.0`).
Provider choices are represented independently.
The current build registers implementations for each stage and rejects
unsupported provider names during startup.

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
- `ASSEMBLYAI_API_KEY`, `SARVAM_API_KEY`, `VOX_STT_PROVIDER`, `VOX_TTS_PROVIDER`

Only the edge (`vox-edge`) has a public domain. It routes `/bridge/*` here, so Twilio and WhatsApp webhooks point at `https://api.voxagent.in/bridge/...`. The service also exposes `/health/live`, `/health/ready` and `/internal/v1/**`, none of which the edge forwards. See `vox-edge/ROUTING.md`.
