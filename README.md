# Vox bridge

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

Run `cargo test` and `cargo check` to verify locally. Tests use a local HTTP
server; they do not call Exa or incur API charges.
