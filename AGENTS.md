# AGENTS.md

## Purpose

This repository owns the `slack-query` and `slack-agent` CLIs and their Codex
skills. Keep retrieval and mutation as separate public contracts.

## Contract

- Keep `slack-query` read-only.
- Keep `slack-agent` writes explicit.
- Preserve compact JSON output for agent use.
- Preserve existing config paths and environment variables until a documented
  migration exists.
- Never commit Slack tokens, workspace identifiers, or unsanitized app URLs.

## Verification

Run `just check` before committing.

## Releases

- Use the `cut-release` skill for normal releases.
- Use `uv run scripts/release.py` as the release entrypoint.
- Run `check`, `plan`, and `run --dry-run` before `run --apply`.
- Pass an exact version for an initial release, prerelease, or build metadata.
- Treat `run --apply` as a public mutation. It commits, pushes, tags, and creates
  a GitHub release.
