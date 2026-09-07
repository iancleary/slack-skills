---
name: slack-agent-cli
description: "Use the `slack-agent` CLI for explicit assistant-oriented Slack reads and writes: thread reads, thread replies, reactions, file uploads, DM setup, and channel join. Use `slack-query` instead when the task is read-only retrieval."
---

# Slack Agent CLI

This skill covers the `slack-agent` binary. Use it when the task requires an explicit assistant-oriented Slack action.

Start narrow:

- `slack-agent thread read <channel-id> <thread-ts> --json`
- `slack-agent reply send <channel-id> <thread-ts> --body-file reply.md --json`
- `slack-agent reaction add <channel-id> <message-ts> eyes --json`
- `slack-agent file upload <channel-id> <thread-ts> ./artifact.txt --title "Artifact" --json`
- `slack-agent dm open <user-id> --json`
- `slack-agent dm send <user-id> --text "..." --json`
- `slack-agent channel join <channel-id> --json`

Working rules:

- Use `--json` for reads and mutation results because the skill is consumed by agents and should stay deterministic and low-token.
- Prefer replying in an existing thread over starting a new top-level conversation.
- Use `--body-file` for longer replies instead of fragile shell quoting.
- Open or reuse a DM when the interaction is user-specific.
- Upload files into the existing thread when the artifact belongs with the conversation.

Safety:

- This CLI mutates Slack state.
- Replies, reactions, uploads, DMs, and channel joins require explicit user intent.
- Do not invent higher-level workflow behavior such as inbox triage or draft approval that is outside the current CLI contract.

Common flow:

1. Read the thread or open the DM needed for context.
2. Confirm the exact action the user wants.
3. Perform the narrow write command.
4. Return the key message, file, or channel identifiers from JSON rather than dumping full payloads.

## Inputs

- channel/thread/message/user identifiers + read vs mutate intent

## Output

- narrow `slack-agent ... --json` commands + key fields summary

## Checks

- prefer reading the thread first before writing into it
