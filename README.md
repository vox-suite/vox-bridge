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
VOX_CORE_SERVICE_TOKEN=replace-with-a-shared-service-token
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
SARVAM_TTS_MODEL=bulbul:v3
SARVAM_LANGUAGE_CODE=en-IN
SARVAM_SPEAKER=shubh
SARVAM_TTS_PACE=1.0
```

Provider, model, language, and speaker choices are represented independently.
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

Run `cargo test --locked`, strict Clippy, and a release build to verify locally.
Tests use local network boundaries and do not call paid provider APIs. Local
verification does not prove the binary is deployed or that a real phone call
works with the deployed credentials.
