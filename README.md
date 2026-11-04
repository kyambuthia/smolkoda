# smolkoda

The smallest useful agentic coding harness, in Rust. One binary, six tools, and
a provider layer that speaks both OpenAI-compatible and Anthropic-compatible
APIs.

```bash
cargo build --release
./target/release/smolkoda --setup
./target/release/smolkoda
```

No Python, no runtime, no SDKs. The only network dependency is the provider you
configure.

## Architecture

Three named pieces do the work:

| Piece | File | Responsibility |
|---|---|---|
| **looper** | `src/looper.rs` | The agentic loop (named after the film). Runs a turn, executes tool calls, and commits the feed only when the turn succeeds. Capped at 50 steps. |
| **feed** | `src/feed.rs` | The timeline. Ordered user / assistant / tool-result events, converted to each provider's message format on demand. |
| **router** | `src/router.rs` | Routes model calls. Normalizes Anthropic Messages and OpenAI Chat Completions, and falls back through configured routes on 429, 5xx, and connection errors. |

Supporting modules:

- `src/presets.rs` — built-in provider presets.
- `src/config.rs` — profiles, env overrides, config file, secret references.
- `src/tools.rs` — `read`, `write`, `edit`, `glob`, `grep`, `bash`.
- `src/setup.rs` — interactive provider wizard.
- `src/ui.rs` — terminal output.

## Provider setup

Run `smolkoda --setup` and pick a preset or choose **custom** for any endpoint
that speaks OpenAI Chat Completions or Anthropic Messages.

Built-in presets:

| Hosted | Local | Compatible |
|---|---|---|
| Anthropic, OpenAI, OpenRouter, RunInfra | Ollama, LM Studio, vLLM, llama.cpp | Google Gemini, DeepSeek, xAI, Groq, Mistral, Cohere, Together, Fireworks, Cerebras, Moonshot, GitHub Models, Azure OpenAI |

Each profile stores its own protocol, base URL, model, auth style, headers, and
API key. Add as many profiles as you like and switch with `/provider NAME`.
When a preset's API key environment variable is available, setup uses it
automatically and stores an `env:NAME` reference, so there is no need to paste
the key into the wizard. Setup queries the provider's `/models` endpoint and
lets you choose from the models it returns. If the endpoint is unavailable,
you can still enter a model ID manually.

RunInfra's preset uses `https://api.runinfra.ai/v1` and reads
`RUNINFRA_GATEWAY_KEY` (or `RUNINFRA_API_KEY`). Its Anthropic-compatible
Messages endpoint is the same base URL; use protocol `anthropic` if you prefer
that wire format.

### Custom endpoints

- **OpenAI-compatible**: protocol `openai`, base URL such as
  `https://gateway.example.com/v1`, auth `bearer`.
- **Anthropic-compatible**: protocol `anthropic`, base URL such as
  `https://proxy.example.com/v1`, auth `x-api-key` or `bearer`.
- **Azure OpenAI**: use the full deployment endpoint including the query string
  (`.../chat/completions?api-version=...`) and auth `api-key`.
- **Local servers**: auth `none`; no API key is stored.

## Config

Default location: `$SMOLKODA_CONFIG`, or
`$XDG_CONFIG_HOME/smolkoda/config.json`, or `~/.config/smolkoda/config.json`.
The file is written with mode `0600`.

```json
{
  "active": "work",
  "providers": {
    "work": {
      "preset": "openrouter",
      "api_key": "env:OPENROUTER_API_KEY",
      "model": "anthropic/claude-sonnet-4.5",
      "fallbacks": ["local"]
    },
    "local": {
      "protocol": "openai",
      "base_url": "http://localhost:11434/v1",
      "auth": "none",
      "api_key": "",
      "model": "qwen3:8b"
    }
  }
}
```

API keys can be literal, `env:NAME`, or `${NAME}`. Prefer env references so the
secret stays out of the file.

Optional profile fields: `headers` (object), `max_tokens`, `max_tokens_param`
(`max_tokens` or `max_completion_tokens`), `temperature`, `timeout` (seconds),
and `fallbacks` (profile names tried in order for retryable failures).

## Commands

- `/provider [name]` — list or switch providers (opens setup if a key is missing)
- `/model [name]` — show or set the model
- `/models` — fetch and list models available from the active provider
- `/config` — show the active route
- `/feed` or `/timeline` — show the feed timeline
- `/tools` — list tools
- `/c` — clear the conversation
- `/q` or `exit` — quit

## CLI

```bash
smolkoda "explain this repo"          # one-shot
smolkoda --provider local             # start with a profile
smolkoda --model gpt-5.2              # override the model
smolkoda --test                       # test the route and exit
smolkoda --list-providers             # show presets and profiles
smolkoda --config ./smolkoda.json     # use another config file
```

Env overrides: `SMOLKODA_PROVIDER`, `SMOLKODA_MODEL`, `SMOLKODA_BASE_URL`,
`SMOLKODA_API_KEY`, `SMOLKODA_PROTOCOL`, `SMOLKODA_MAX_TOKENS`,
`SMOLKODA_TEMPERATURE`, `SMOLKODA_TIMEOUT`, `SMOLKODA_CONFIG`.

## Tools

| Tool | Description |
|---|---|
| `read` | Read a text file with line numbers and 1-based offset/limit |
| `write` | Write a file, creating parent directories |
| `edit` | Replace a unique string, or all occurrences with `all=true` |
| `glob` | Find files by pattern, newest first |
| `grep` | Regex search, max 100 hits, skips build/VCS directories |
| `bash` | Stream shell output, 30s default timeout, 40k character cap |

## Notes

- The harness uses native tool calling. The model/provider must support it.
- Tools run with your user's permissions: `bash` can execute anything and `write`/`edit` can change any file you can. Use a container or VM for untrusted repositories.
- API keys are stored as plaintext unless you use `env:NAME` or `${NAME}`. The config file is written with mode `0600`.
- `prototype/smolkoda.py` is the original single-file Python prototype; the Rust binary is the main implementation.
- The project is intentionally small. Read the modules in order:
  `feed` → `router` → `looper`.
