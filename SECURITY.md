# Security Policy

## Supported versions

Security fixes are accepted for the current `main` branch unless a release
branch policy is published by maintainers.

## Reporting a vulnerability

Please report suspected vulnerabilities through the repository's private
security advisory channel when available. If no advisory channel is configured,
contact the maintainers privately before opening a public issue.

Do not include secrets, bearer tokens, non-public hostnames, non-public repository
paths, corpus content that cannot be redistributed, or complete exploit payloads
in public reports.

## Security expectations

- MCP calls to `/mcp` must require bearer authentication.
- `/health`, `/attest`, and OAuth discovery metadata are public by design.
- Non-loopback binding requires explicit operator opt-in.
- Generated indexes, replay stores, and semantic artifacts may contain derived
  corpus content and should be protected like the source material.
- File and hover inputs must remain constrained to configured corpus or
  workspace roots.
