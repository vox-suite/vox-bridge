# Vox bridge

## Voice calls

Inbound Twilio calls use a bidirectional Media Stream through a configurable
voice pipeline:

`Twilio -> AssemblyAI -> Gemini -> Sarvam -> Twilio`

The Twilio adapter accepts μ-law audio at 8000 Hz. AssemblyAI transcribes the
incoming stream, finalized turns are sent to the existing Gemini agent, and
Sarvam streams the response back as μ-law audio at 8000 Hz without
transcoding. Caller speech interrupts active playback.

Set the required credentials in the local `.env` and in the deployed service
environment:

```dotenv
TWILIO_AUTH_TOKEN=your-twilio-auth-token
ASSEMBLYAI_API_KEY=your-assemblyai-api-key
GEMINI_API_KEY=your-gemini-api-key
SARVAM_API_KEY=your-sarvam-api-key
EXA_API_KEY=your-exa-api-key
```

The default provider profile can be stated explicitly:

```dotenv
VOX_STT_PROVIDER=assemblyai
ASSEMBLYAI_SPEECH_MODEL=universal-3-5-pro
VOX_AGENT_PROVIDER=gemini
GEMINI_MODEL=gemini-3.5-flash-lite
VOX_TTS_PROVIDER=sarvam
SARVAM_TTS_MODEL=bulbul:v3
SARVAM_LANGUAGE_CODE=en-IN
SARVAM_SPEAKER=shubh
SARVAM_TTS_PACE=1.0
```

Provider, model, language, and speaker choices are represented independently.
The current build registers one implementation for each stage and rejects
unsupported provider names during startup.

Configure Twilio Voice to send incoming call webhooks to:

`POST https://api.voxagent.in/bridge/twilio/voice`

The returned TwiML connects Twilio to:

`wss://api.voxagent.in/bridge/twilio/voice/stream`

Both requests validate `X-Twilio-Signature` before a call session is accepted.

## Web search

The Gemini agent uses Exa through the `web_search` tool. SearXNG is not required.

Set `EXA_API_KEY` in your local `.env` and in the environment of the deployed
bridge service, alongside the existing `GEMINI_API_KEY`. Obtain a key from
https://dashboard.exa.ai/api-keys. Keep keys out of version control.

```dotenv
EXA_API_KEY=your-exa-api-key
```

Restart the bridge after changing its environment. A missing Exa key produces a
configuration error before the agent runs.

Search uses `POST https://api.exa.ai/search`, `type: auto`, five results, and
highlights. Requests time out after 15 seconds. The agent has up to ten model
turns to search and answer with source URLs. API errors are reported without
exposing response bodies or credentials.

API reference: https://exa.ai/docs/reference/search

Run `cargo test --locked`, strict Clippy, and a release build to verify locally.
Tests use local network boundaries and do not call paid provider APIs. Local
verification does not prove the binary is deployed or that a real phone call
works with the deployed credentials.
