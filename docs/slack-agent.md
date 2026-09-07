# Slack Agent CLI

This document defines the stricter read/write Slack CLI used by assistant-specific workflows.

## Goal

Provide a smaller, workflow-oriented command surface for assistants such as OpenClaw or Hermes so agent runs are faster, less ambiguous, and less likely to fail due to tool misuse, timeouts, or oversized outputs.

This is not the shared Slack utility layer. It sits on top of that layer or parallel to it with tighter workflow contracts.

## Current State

Status:

- crate exists and is wired into this workspace
- current implementation is a narrow explicit write surface, not a full assistant workflow engine
- config lives under `~/.config/forge/slack-agent/`
- auth supports either a bot token or user token, with bot preferred

Implemented commands:

- `auth login`
- `thread read`
- `reply send`
- `reaction add`
- `file upload`
- `file info`
- `dm open`
- `dm send`
- `channel join`

Not implemented yet:

- inbox triage
- mention queues
- draft/review workflows
- thread summarization
- batching or approval orchestration
- permalink-based convenience commands

## Why Separate It

`slack-query` is for reusable Slack primitives across many Codex sessions.

`slack-agent` is for assistant-aligned actor workflows:

- read a thread with file metadata and reactions
- reply into an existing thread
- open or seed a DM with a configured user
- add reactions
- upload files into a thread
- perform a small number of explicit conversation setup actions such as `channel join`

Separating them keeps the general CLI stable while letting assistant actors use stricter commands and smaller outputs.

## Design Outcome

This is the current result of the design pass.

Requirement that survived:

- An assistant needs a Slack surface that can both read and write, while preserving conversational context by strongly preferring replies in threads.

What was deleted or refused:

- broad “do everything in Slack” scope
- implicit root posting in channels
- workflow-heavy inbox/triage abstractions before the primitive write path is stable
- a first-class `voice` or `voice-message` resource

Narrowest remaining contract:

- read threads
- reply to threads
- open and seed DMs
- react to messages
- upload and inspect files
- perform a small number of explicit setup actions such as joining a channel

Why:

- this is enough to let an assistant instance converse with a configured Slack user
- it keeps the mutation surface explicit
- it avoids inventing Slack-specific abstractions that the public API does not clearly expose

## Setup

Use Slack App Settings under `OAuth & Permissions` to configure the app.

Do not store private app-settings URLs with raw workspace or app IDs in the repo. If you need an example route, sanitize it:

```text
https://app.slack.com/app-settings/<workspace-id>/<app-id>/oauth
```

## Safety Model

Suggested action tiers:

- Tier 1: read and summarize
- Tier 2: explicit send into an existing thread
- Tier 3: explicit root sends only for DM setup
- Tier 4: destructive or administrative actions

Default behavior:

- reads are allowed
- sends require an explicit send command
- thread replies are the default write path
- non-thread root sends are intentionally narrow and only supported through `dm send`
- destructive/admin actions should be excluded unless there is a strong proven need

Thread-first rule:

- if a thread already exists, reply in the thread
- if no thread exists and the interaction is user-specific, prefer opening or reusing a DM
- avoid top-level channel posting unless a later explicit contract proves it is necessary

## Permission Scopes

Supported token types:

- bot token
- user token

Preference:

- prefer a bot token by default so the assistant has an explicit actor identity
- allow a user token when the local Slack install is intentionally user-aligned

Recommended baseline scopes for the initial `slack-agent` implementation:

- `channels:history`
- `groups:history`
- `im:history`
- `mpim:history`
- `chat:write`
- `reactions:write`
- `files:write`
- `files:read`
- `im:write`

Optional only when the install needs them:

- `channels:join`
- `chat:write.public`

Avoid by default:

- broad channel management scopes
- reminder scopes
- unrelated write scopes

## Voice And Audio Decision

Slack agents should not currently model voice as a first-class Slack resource.

Reason:

- the documented Slack API surface clearly exposes generic message, thread, DM, reaction, and file operations
- the documented Slack API surface does not clearly expose a separate first-class voice-message resource with dedicated create/read methods

Current interpretation:

- reading a voice note means reading a message and its attached file metadata
- uploading a voice note means uploading an audio file into a thread or DM using the generic file API
- this CLI should treat “voice” as message-plus-file until Slack exposes a stable dedicated API or repeated agent usage proves a higher-level abstraction is needed

Implication for command design:

- keep `file upload` and `file info` generic
- do not add `voice read`, `voice upload`, or `voice info` yet
- if later needed, add thin aliases on top of file/message primitives rather than inventing a separate storage model

## Candidate Commands

Initial command surface:

```sh
slack-agent auth login --token xoxb-... --token-type bot --force
slack-agent thread read C123 1712785154.123456 --json
slack-agent reply send C123 1712785154.123456 --body-file reply.md --json
slack-agent reaction add C123 1712785154.123456 eyes --json
slack-agent file upload C123 1712785154.123456 ./diagram.png --title "Diagram" --json
slack-agent file info F123 --json
slack-agent dm open U123 --json
slack-agent dm send U123 --text "Started a new thread." --json
slack-agent channel join C123 --json
```

Design requirements:

- bounded output size
- explicit defaults
- no hidden sending behavior
- thread replies should be the normal send path
- retryable structured errors
- small result schemas tailored to actor workflows

State note:

- this command list reflects the currently implemented crate surface
- richer assistant workflows remain intentionally deferred until the primitive contract proves stable

## Relationship To `slack-query`

`slack-query` exposes reusable research primitives such as:

- permalink parsing
- thread reads
- context reads
- search

`slack-agent` exposes narrower assistant actions such as:

- thread-first sending
- DM setup
- reactions
- file upload
- explicit conversation setup

Token boundary:

- `slack-query` stays user-aligned by default
- `slack-agent` prefers a bot token but can also run with a user token when explicitly configured

## Scope Boundary

`slack-agent` is in scope for design-sequence review because it is a contract-shaping problem, not just an implementation task.

Applied outcome:

- keep the CLI narrow
- prefer explicit verbs
- avoid speculative resources and workflow layers
- automate only after the primitive surface is necessary and stable

## Auth Configuration

Preferred auth layout:

```text
~/.config/forge/slack-agent/
  config.toml
  token
```

Supported lookup order:

1. `SLACK_AGENT_API_TOKEN`
2. `~/.config/forge/slack-agent/config.toml`
3. `~/.config/forge/slack-agent/token`

Optional override:

- `FORGE_SLACK_AGENT_CONFIG_DIR`

Recommended `config.toml`:

```toml
token_file = "~/.config/forge/slack-agent/token"
token_type = "bot"
```

Recommended local permissions:

- config directory: `0700`
- token and config files: `0600`
