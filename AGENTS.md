# Agent guidance

This repository is a small Rust CLI. Keep changes minimal, idiomatic, and
consistent with the existing module layout.

## Layout

- `src/main.rs` — argument parsing and the REPL.
- `src/feed.rs` — conversation timeline and provider message conversion.
- `src/router.rs` — provider requests, fallbacks, and response parsing.
- `src/looper.rs` — the agent tool loop.
- `src/tools.rs` — `read`, `write`, `edit`, `glob`, `grep`, and `bash`.
- `src/config.rs` and `src/presets.rs` — profiles and provider presets.
- `src/setup.rs` and `src/ui.rs` — interactive setup and terminal output.
- `src/voice.rs` — `/voice` recording and speech playback.

## Rules

- Preserve the declared MSRV in `Cargo.toml`; do not use newer standard-library
  APIs without raising it deliberately.
- Use `std::process::Command` and `cfg!(windows)`/`cfg(target_os = ...)` for
  platform behavior. Do not introduce shell syntax that only works on Unix into
  release packaging steps.
- Keep secrets out of logs and test fixtures. Use `env:NAME` or `${NAME}` for
  key references.
- Add focused unit tests for config resolution, response parsing, and file tools
  when changing them. Network calls should not be required by the test suite.
- Run the checks from `CONTRIBUTING.md` before finishing.
- Update `CHANGELOG.md` and `README.md` when behavior or installation changes.
