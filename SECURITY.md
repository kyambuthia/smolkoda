# Security policy

## Reporting a vulnerability

Please report security issues privately through GitHub's security advisory
form at:

https://github.com/kyambuthia/smolkoda/security/advisories/new

Do not open a public issue for a vulnerability before it has been addressed.
Include the affected version, a description of the impact, and minimal
reproduction steps. Avoid including real API keys or other secrets in reports.

## Supported versions

Security fixes are made on `main` and included in the next release. When a fix
is practical to backport, the latest released minor version receives it.

## Security model

smolkoda is a local coding tool. It runs shell commands and writes files with
your user's permissions, so only use it with repositories and providers you
trust. API keys are stored in the config file unless referenced through
`env:NAME` or `${NAME}`; the config file is created with mode `0600` on Unix.
