# slack-skills

Agent-friendly Slack CLIs and their matching skills.

This repository keeps two command surfaces separate:

- `slack-query` provides deterministic, read-only retrieval.
- `slack-agent` provides explicit assistant-oriented reads and writes.

Both commands use compact JSON envelopes with `--json`. The shared Rust crates
keep authentication, Slack API parsing, and error behavior consistent without
merging the read-only and mutating interfaces.

## Install the CLIs

```sh
cargo install --git https://github.com/iancleary/slack-skills --locked slack-query slack-agent
```

## Install the skills

```sh
npx skills add iancleary/slack-skills -g
```

## Existing configuration

The initial extraction preserves the existing configuration contract:

- `~/.config/forge/slack-query/`
- `~/.config/forge/slack-agent/`
- `FORGE_SLACK_QUERY_CONFIG_DIR`
- `FORGE_SLACK_AGENT_CONFIG_DIR`
- `FORGE_CONFIG_DIR` for `slack-agent`

Existing tokens continue to work. A future config migration belongs in this
repository and must retain a documented compatibility path.

## Development

```sh
just check
```

Do not use live Slack credentials in tests. The test suite validates command and
output behavior without contacting a workspace.
