# Contributing to smolkoda

Thanks for helping improve smolkoda. The project is intentionally small; please
keep changes focused and avoid adding dependencies unless they are necessary.

## Development setup

Install Rust 1.88 or newer, then clone the repository and build:

```bash
git clone https://github.com/kyambuthia/smolkoda.git
cd smolkoda
cargo build
```

`cargo build` also works with the oldest supported compiler declared by
`rust-version` in `Cargo.toml`. Keep the crate compatible with that MSRV; CI
checks it separately.

## Before opening a pull request

Run the same checks as CI:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked --all-targets
```

For release-workflow changes, validate the workflow YAML and describe the
affected targets in the pull request. Release jobs build on Linux, macOS, and
Windows, so conditional steps must remain valid on their runner shell.

## Commits and pull requests

- Use conventional-style subjects such as `fix(release): ...` or `feat(voice): ...`.
- Keep each pull request to one coherent change.
- Add or update tests when changing observable behavior without requiring a
  network service.
- Update `CHANGELOG.md` for user-visible changes.
- Do not include API keys, local configuration files, or generated binaries.

## Release checks

Releases are created by pushing a `v*` tag. Before tagging, make sure `main` is
green in CI, `Cargo.toml` and `CHANGELOG.md` agree on the version, and the
release workflow has successfully built all targets for the previous tag.
