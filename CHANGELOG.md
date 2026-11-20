# Changelog

## Unreleased

- Add CI for formatting, Clippy, tests, and the declared Rust 1.88 MSRV.
- Add contributor, security, and agent guidance plus build/test documentation.
- Add tests for provider configuration, response parsing, and file tools.
- Add scrollable terminal pickers for provider and model selection.
- Consolidate model selection under `/model`; the separate `/models` command is removed.
- Show model pricing and cost tiers (`free`, `cheap`, `standard`, `expensive`) in setup and `/model` when provided by `/models`, and add `/cost` for session token usage and cost.
- Use the operating system certificate store for HTTPS provider connections.
- Fix the documented install command to use the Git repository until the crate is published.

## 0.1.0

- Voice mode: `/voice [seconds]` records a spoken prompt, transcribes it (OpenRouter Whisper), runs the agent turn, and speaks the reply (Fish Audio).

## 0.0.1

Initial release of smolkoda as a Rust command-line coding harness.

- OpenAI Chat Completions and Anthropic Messages provider protocols, with configurable provider profiles and fallback routes.
- Six tools for reading, writing, editing, finding files, searching text, and running shell commands.
- Interactive setup, model discovery, one-shot prompts, and an interactive terminal session.
