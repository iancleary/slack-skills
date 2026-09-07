---
name: slack-query-cli
description: "Use the `slack-query` CLI for deterministic Slack retrieval: permalink resolution, message search, thread reads, channel context, and thread context. Prefer this over mutating Slack tools when the task is read-only."
---

# Slack Query CLI

This skill covers the `slack-query` binary. Use it when the task is Slack retrieval or research.

Start narrow:

- `slack-query resolve-permalink <url> --json`
- `slack-query search <query> --limit 5 --json`
- `slack-query read-thread <channel-id> <thread-ts> --limit 15 --json`
- `slack-query channel-context <channel-id> <message-ts> --before 3 --after 3 --json`
- `slack-query thread-context <channel-id> <thread-ts> <message-ts> --before 3 --after 3 --json`

Working rules:

- Use `--json` for retrieval because the skill is consumed by agents and should stay deterministic and low-token.
- Resolve a permalink first when the user provides a Slack URL.
- Search with a small limit first, then expand only if needed.
- Use `read-thread` for the whole conversation and `thread-context` when only local context around one reply is needed.
- Use `channel-context` for top-level neighborhood around a message in the channel timeline.

Safety:

- Treat this CLI as read-only.
- Do not invent write behaviors that are outside the current CLI contract.

Common flow:

1. Resolve a permalink or search for the target.
2. Reuse returned `channel_id`, `message_ts`, and `thread_ts`.
3. Pull the smallest useful context.
4. Summarize the relevant Slack evidence.

## Inputs

- permalink or query + desired context window

## Output

- narrow `slack-query ... --json` commands + evidence summary

## Checks

- keep `--limit` small first; expand only if needed
