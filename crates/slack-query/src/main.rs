use std::{env, fmt::Write as _, path::PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use slack_core::{
    ErrorBody, OutputMode, SlackMessage, classify_slack_error_code,
    config_dir_path as shared_config_dir_path, emit_output, format_error_human, normalize_token,
    prepare_config_dir, print_error_json, prompt_for_token, read_history_messages,
    read_thread_messages, read_token as shared_read_token, slack_client as shared_slack_client,
    slack_get, write_token_file,
};
use url::Url;

#[derive(Parser, Debug)]
#[command(name = "slack-query", version)]
#[command(about = "Slack query CLI for deterministic research and retrieval workflows")]
#[command(
    after_help = "Output:\n  - Default output is human-readable.\n  - Use --json for compact machine-readable JSON.\n  - Errors follow the same rule: human-readable by default, compact JSON with --json."
)]
struct Cli {
    #[arg(long, global = true, help = "Emit compact machine-readable JSON")]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(subcommand)]
    Auth(AuthCommand),
    ResolvePermalink(ResolvePermalinkArgs),
    ReadThread(ReadThreadArgs),
    #[command(name = "channel-context")]
    ChannelContext(ContextArgs),
    ThreadContext(ThreadContextArgs),
    Search(SearchArgs),
}

#[derive(Subcommand, Debug)]
enum AuthCommand {
    Login(AuthLoginArgs),
}

#[derive(Args, Debug)]
struct AuthLoginArgs {
    #[arg(long, help = "Slack API token to save instead of prompting")]
    token: Option<String>,
    #[arg(long, help = "Overwrite an existing token file")]
    force: bool,
}

#[derive(Args, Debug)]
struct ResolvePermalinkArgs {
    #[arg(help = "Slack message permalink")]
    permalink: String,
}

#[derive(Args, Debug)]
struct ReadThreadArgs {
    #[arg(help = "Slack channel ID, such as C123")]
    channel_id: String,
    #[arg(help = "Thread root timestamp")]
    thread_ts: String,
    #[arg(
        long,
        default_value_t = 15,
        help = "Maximum number of thread messages to return"
    )]
    limit: u32,
}

#[derive(Args, Debug)]
struct ContextArgs {
    #[arg(help = "Slack channel ID, such as C123")]
    channel_id: String,
    #[arg(help = "Target message timestamp in the parent channel timeline")]
    message_ts: String,
    #[arg(
        long,
        default_value_t = 5,
        help = "Number of earlier top-level channel messages"
    )]
    before: u32,
    #[arg(
        long,
        default_value_t = 5,
        help = "Number of later top-level channel messages"
    )]
    after: u32,
}

#[derive(Args, Debug)]
struct ThreadContextArgs {
    #[arg(help = "Slack channel ID, such as C123")]
    channel_id: String,
    #[arg(help = "Thread root timestamp")]
    thread_ts: String,
    #[arg(help = "Target message timestamp inside the thread")]
    message_ts: String,
    #[arg(long, default_value_t = 5, help = "Number of earlier thread messages")]
    before: u32,
    #[arg(long, default_value_t = 5, help = "Number of later thread messages")]
    after: u32,
}

#[derive(Args, Debug)]
struct SearchArgs {
    #[arg(help = "Slack search query")]
    query: String,
    #[arg(
        long,
        default_value_t = 20,
        help = "Maximum number of matches to request"
    )]
    limit: u32,
    #[arg(long, default_value_t = 1, help = "Search results page number")]
    page: u32,
    #[arg(
        long,
        default_value = "timestamp",
        help = "Sort key, usually timestamp or score"
    )]
    sort: String,
    #[arg(long, default_value = "desc", help = "Sort direction: asc or desc")]
    sort_dir: String,
}

#[derive(Debug, Serialize)]
struct AuthLoginResult {
    token_file: String,
    created: bool,
}

#[derive(Debug, Serialize)]
struct ResolvedPermalink {
    team_domain: String,
    channel_id: String,
    message_ts: String,
    thread_ts: String,
    is_thread_root: bool,
    reply_count: Option<u32>,
}

#[derive(Debug, Serialize)]
struct ThreadResult {
    channel_id: String,
    thread_ts: String,
    messages: Vec<Message>,
    response_metadata: ResponseMetadata,
}

#[derive(Debug, Serialize)]
struct ContextResult {
    channel_id: String,
    target: Message,
    before: Vec<Message>,
    after: Vec<Message>,
    response_metadata: ResponseMetadata,
}

#[derive(Debug, Serialize)]
struct ResponseMetadata {
    rate_limited: bool,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct SearchResult {
    query: String,
    messages: Vec<SearchMatch>,
    response_metadata: ResponseMetadata,
}

#[derive(Debug, Serialize, Deserialize)]
struct SearchMatch {
    #[serde(default)]
    iid: Option<String>,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    channel: Option<SearchChannel>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    text: String,
    ts: String,
    #[serde(default)]
    permalink: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SearchChannel {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
struct Message {
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    text: String,
    ts: String,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    reply_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SlackSearchResponse {
    messages: Option<SlackSearchMessages>,
}

#[derive(Debug, Deserialize)]
struct SlackSearchMessages {
    matches: Vec<SearchMatch>,
    #[serde(default)]
    pagination: Option<SlackPagination>,
}

#[derive(Debug, Deserialize)]
struct SlackPagination {
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    page_count: Option<u32>,
}

#[tokio::main]
async fn main() {
    let args = env::args_os().collect::<Vec<_>>();
    let wants_json = args.iter().any(|arg| arg.to_str() == Some("--json"));
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let exit_code = err.exit_code();
            match err.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                    let _ = err.print();
                }
                _ if wants_json => {
                    print_error_json(&ErrorBody {
                        code: "invalid_usage".to_string(),
                        message: err.to_string(),
                    });
                }
                _ => {
                    let _ = err.print();
                }
            }
            std::process::exit(exit_code);
        }
    };
    let output = OutputMode::from_json_flag(cli.json);

    let result = run(cli).await;
    if let Err(err) = result {
        let cli_error = classify_error(&err);
        match output {
            OutputMode::Json => print_error_json(&cli_error),
            OutputMode::Human => eprintln!("{}", format_error_human("slack-query", &cli_error)),
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let output = OutputMode::from_json_flag(cli.json);
    match cli.command {
        Command::Auth(AuthCommand::Login(args)) => {
            let data = login(args)?;
            emit_output(output, data, format_auth_login_human)?;
        }
        Command::ResolvePermalink(args) => {
            let data = resolve_permalink(&args.permalink)?;
            emit_output(output, data, format_resolve_permalink_human)?;
        }
        Command::ReadThread(args) => {
            let token = read_token()?;
            let client = slack_client(&token)?;
            let data = read_thread(&client, &args.channel_id, &args.thread_ts, args.limit).await?;
            emit_output(output, data, format_thread_result_human)?;
        }
        Command::ChannelContext(args) => {
            let token = read_token()?;
            let client = slack_client(&token)?;
            let data = read_channel_context(
                &client,
                &args.channel_id,
                &args.message_ts,
                args.before,
                args.after,
            )
            .await?;
            emit_output(output, data, |data| {
                format_context_result_human("channel-context", data)
            })?;
        }
        Command::ThreadContext(args) => {
            let token = read_token()?;
            let client = slack_client(&token)?;
            let data = read_thread_context(
                &client,
                &args.channel_id,
                &args.thread_ts,
                &args.message_ts,
                args.before,
                args.after,
            )
            .await?;
            emit_output(output, data, |data| {
                format_context_result_human("thread-context", data)
            })?;
        }
        Command::Search(args) => {
            let token = read_token()?;
            let client = slack_client(&token)?;
            let data = search_messages(
                &client,
                &args.query,
                args.limit,
                args.page,
                &args.sort,
                &args.sort_dir,
            )
            .await?;
            emit_output(output, data, format_search_result_human)?;
        }
    }

    Ok(())
}

fn format_auth_login_human(result: &AuthLoginResult) -> String {
    format!(
        "slack-query auth login: wrote token file\npath: {}",
        result.token_file
    )
}

fn format_resolve_permalink_human(result: &ResolvedPermalink) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "slack-query resolve-permalink");
    let _ = writeln!(out, "team: {}", result.team_domain);
    let _ = writeln!(out, "channel: {}", result.channel_id);
    let _ = writeln!(out, "message_ts: {}", result.message_ts);
    let _ = writeln!(out, "thread_ts: {}", result.thread_ts);
    let _ = writeln!(out, "thread_root: {}", result.is_thread_root);
    if let Some(reply_count) = result.reply_count {
        let _ = writeln!(out, "reply_count: {reply_count}");
    }
    out.trim_end().to_string()
}

fn format_thread_result_human(result: &ThreadResult) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "slack-query read-thread: {} {} ({} messages)",
        result.channel_id,
        result.thread_ts,
        result.messages.len()
    );
    append_response_metadata(&mut out, &result.response_metadata);
    out.push('\n');
    append_messages(&mut out, &result.messages);
    out.trim_end().to_string()
}

fn format_context_result_human(command: &str, result: &ContextResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "slack-query {command}: {}", result.channel_id);
    append_response_metadata(&mut out, &result.response_metadata);
    out.push('\n');
    let _ = writeln!(out, "target:");
    append_message_block(&mut out, &result.target, "  ");
    if !result.before.is_empty() {
        let _ = writeln!(out, "before:");
        append_messages_with_indent(&mut out, &result.before, "  ");
    }
    if !result.after.is_empty() {
        let _ = writeln!(out, "after:");
        append_messages_with_indent(&mut out, &result.after, "  ");
    }
    out.trim_end().to_string()
}

fn format_search_result_human(result: &SearchResult) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "slack-query search: {} matches for {:?}",
        result.messages.len(),
        result.query
    );
    append_response_metadata(&mut out, &result.response_metadata);
    for (idx, message) in result.messages.iter().enumerate() {
        let channel_name = message
            .channel
            .as_ref()
            .and_then(|channel| channel.name.as_deref())
            .unwrap_or("-");
        let channel_id = message
            .channel
            .as_ref()
            .and_then(|channel| channel.id.as_deref())
            .unwrap_or("-");
        let author = message
            .username
            .as_deref()
            .or(message.user.as_deref())
            .unwrap_or("-");
        let _ = writeln!(
            out,
            "{}. {} [{}] {} {}",
            idx + 1,
            channel_name,
            channel_id,
            message.ts,
            author
        );
        let preview = preview_human_text(&message.text, 180);
        let _ = writeln!(out, "   {}", preview);
        if let Some(permalink) = message.permalink.as_deref() {
            let _ = writeln!(out, "   permalink: {permalink}");
        }
    }
    out.trim_end().to_string()
}

fn append_response_metadata(out: &mut String, metadata: &ResponseMetadata) {
    let _ = writeln!(out, "rate_limited: {}", metadata.rate_limited);
    if let Some(cursor) = metadata.next_cursor.as_deref() {
        let _ = writeln!(out, "next_cursor: {cursor}");
    }
}

fn append_messages(out: &mut String, messages: &[Message]) {
    append_messages_with_indent(out, messages, "");
}

fn append_messages_with_indent(out: &mut String, messages: &[Message], indent: &str) {
    for message in messages {
        append_message_block(out, message, indent);
    }
}

fn append_message_block(out: &mut String, message: &Message, indent: &str) {
    let author = message.user.as_deref().unwrap_or("-");
    let subtype = message.subtype.as_deref().unwrap_or("message");
    let _ = writeln!(out, "{}[{}] {} {}", indent, message.ts, author, subtype);
    for line in preview_human_text(&message.text, 500).lines() {
        let _ = writeln!(out, "{}  {}", indent, line);
    }
}

fn preview_human_text(text: &str, max_chars: usize) -> String {
    let single_line = text.replace('\n', " ").trim().to_string();
    if single_line.chars().count() <= max_chars {
        single_line
    } else {
        let preview = single_line.chars().take(max_chars).collect::<String>();
        format!("{preview}...")
    }
}

fn read_token() -> Result<String> {
    shared_read_token(
        "SLACK_QUERY_API_TOKEN",
        "FORGE_SLACK_QUERY_CONFIG_DIR",
        "slack-query",
        false,
        "missing Slack query auth; set SLACK_QUERY_API_TOKEN or create ~/.config/forge/slack-query/config.toml or ~/.config/forge/slack-query/token",
    )
}

fn login(args: AuthLoginArgs) -> Result<AuthLoginResult> {
    let config_dir = config_dir_path()?;
    prepare_config_dir(&config_dir)?;

    let token = match args.token {
        Some(token) => token,
        None => prompt_for_token("Paste Slack API token: ")?,
    };
    let token = normalize_token(token)?;

    let token_file = write_token_file(&config_dir, &token, args.force)?;

    Ok(AuthLoginResult {
        token_file: token_file.display().to_string(),
        created: true,
    })
}

fn config_dir_path() -> Result<PathBuf> {
    shared_config_dir_path("FORGE_SLACK_QUERY_CONFIG_DIR", "slack-query", false)
}

fn slack_client(token: &str) -> Result<Client> {
    shared_slack_client(token, "slack-skills/slack-query")
}

fn resolve_permalink(permalink: &str) -> Result<ResolvedPermalink> {
    let url = Url::parse(permalink).context("invalid Slack permalink")?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Slack permalink is missing a host"))?;
    let mut segments = url
        .path_segments()
        .ok_or_else(|| anyhow!("Slack permalink path is invalid"))?;

    let archives = segments
        .next()
        .ok_or_else(|| anyhow!("missing archives segment"))?;
    if archives != "archives" {
        bail!("unsupported Slack permalink path");
    }

    let channel_id = segments
        .next()
        .ok_or_else(|| anyhow!("missing channel id in permalink"))?
        .to_string();
    let raw_message = segments
        .next()
        .ok_or_else(|| anyhow!("missing message id in permalink"))?;
    let message_ts = parse_permalink_ts(raw_message)?;
    let thread_ts = url
        .query_pairs()
        .find(|(key, _)| key == "thread_ts")
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| message_ts.clone());
    let is_thread_root = thread_ts == message_ts;

    Ok(ResolvedPermalink {
        team_domain: host.to_string(),
        channel_id,
        message_ts,
        thread_ts,
        is_thread_root,
        reply_count: None,
    })
}

fn parse_permalink_ts(raw: &str) -> Result<String> {
    let trimmed = raw
        .strip_prefix('p')
        .ok_or_else(|| anyhow!("message segment must start with 'p'"))?;
    if trimmed.len() < 7 || !trimmed.chars().all(|c| c.is_ascii_digit()) {
        bail!("message segment must be numeric after 'p'");
    }
    let split_at = trimmed.len() - 6;
    Ok(format!("{}.{}", &trimmed[..split_at], &trimmed[split_at..]))
}

async fn read_thread(
    client: &Client,
    channel_id: &str,
    thread_ts: &str,
    limit: u32,
) -> Result<ThreadResult> {
    let payload = read_thread_messages(client, channel_id, thread_ts, limit).await?;

    Ok(ThreadResult {
        channel_id: channel_id.to_string(),
        thread_ts: thread_ts.to_string(),
        messages: payload.messages.into_iter().map(Message::from).collect(),
        response_metadata: ResponseMetadata {
            rate_limited: false,
            next_cursor: payload.next_cursor,
        },
    })
}

async fn read_channel_context(
    client: &Client,
    channel_id: &str,
    message_ts: &str,
    before: u32,
    after: u32,
) -> Result<ContextResult> {
    let target_window = fetch_history(
        client,
        channel_id,
        Some(message_ts),
        Some(message_ts),
        true,
        1,
    )
    .await?;
    let before_window = if before > 0 {
        fetch_history(client, channel_id, None, Some(message_ts), false, before).await?
    } else {
        HistoryWindow::default()
    };
    let after_window = if after > 0 {
        fetch_history(client, channel_id, Some(message_ts), None, false, after).await?
    } else {
        HistoryWindow::default()
    };

    let target = target_window
        .messages
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("failed to isolate target message"))?;
    let mut before_messages = before_window.messages;
    before_messages.sort_by(|left, right| left.ts.cmp(&right.ts));
    let mut after_messages = after_window.messages;
    after_messages.sort_by(|left, right| left.ts.cmp(&right.ts));

    Ok(ContextResult {
        channel_id: channel_id.to_string(),
        target,
        before: before_messages,
        after: after_messages,
        response_metadata: ResponseMetadata {
            rate_limited: target_window.rate_limited
                || before_window.rate_limited
                || after_window.rate_limited,
            next_cursor: after_window
                .next_cursor
                .or(before_window.next_cursor)
                .or(target_window.next_cursor),
        },
    })
}

async fn read_thread_context(
    client: &Client,
    channel_id: &str,
    thread_ts: &str,
    message_ts: &str,
    before: u32,
    after: u32,
) -> Result<ContextResult> {
    let thread = read_thread(client, channel_id, thread_ts, before + after + 50).await?;
    let target_index = thread
        .messages
        .iter()
        .position(|message| message.ts == message_ts)
        .ok_or_else(|| anyhow!("target message was not returned in thread"))?;

    let start = target_index.saturating_sub(before as usize);
    let end = (target_index + after as usize + 1).min(thread.messages.len());

    let target = thread
        .messages
        .get(target_index)
        .cloned()
        .ok_or_else(|| anyhow!("failed to isolate target thread message"))?;
    let before_messages = thread.messages[start..target_index].to_vec();
    let after_messages = thread.messages[target_index + 1..end].to_vec();

    Ok(ContextResult {
        channel_id: channel_id.to_string(),
        target,
        before: before_messages,
        after: after_messages,
        response_metadata: thread.response_metadata,
    })
}

#[derive(Default)]
struct HistoryWindow {
    messages: Vec<Message>,
    rate_limited: bool,
    next_cursor: Option<String>,
}

async fn fetch_history(
    client: &Client,
    channel_id: &str,
    oldest: Option<&str>,
    latest: Option<&str>,
    inclusive: bool,
    limit: u32,
) -> Result<HistoryWindow> {
    let payload =
        read_history_messages(client, channel_id, oldest, latest, inclusive, limit).await?;

    Ok(HistoryWindow {
        messages: payload.messages.into_iter().map(Message::from).collect(),
        rate_limited: false,
        next_cursor: payload.next_cursor,
    })
}

async fn search_messages(
    client: &Client,
    query: &str,
    limit: u32,
    page: u32,
    sort: &str,
    sort_dir: &str,
) -> Result<SearchResult> {
    let payload: SlackSearchResponse = slack_get(
        client,
        "search.messages",
        &[
            ("query".to_string(), query.to_string()),
            ("count".to_string(), limit.to_string()),
            ("page".to_string(), page.to_string()),
            ("sort".to_string(), sort.to_string()),
            ("sort_dir".to_string(), sort_dir.to_string()),
        ],
    )
    .await?;

    let messages = payload.messages.unwrap_or(SlackSearchMessages {
        matches: Vec::new(),
        pagination: None,
    });
    let next_cursor =
        messages.pagination.and_then(
            |pagination| match (pagination.page, pagination.page_count) {
                (Some(current), Some(total)) if current < total => Some((current + 1).to_string()),
                _ => None,
            },
        );

    Ok(SearchResult {
        query: query.to_string(),
        messages: messages.matches,
        response_metadata: ResponseMetadata {
            rate_limited: false,
            next_cursor,
        },
    })
}

fn classify_error(error: &anyhow::Error) -> ErrorBody {
    let message = error.to_string();
    let code = match message.as_str() {
        msg if msg.contains("missing Slack query auth") => "auth_missing",
        "invalid_query" => "validation_error",
        msg if msg.contains("token file already exists") => "validation_error",
        msg if msg.contains("empty Slack token") => "validation_error",
        _ => classify_slack_error_code(&message).unwrap_or("internal_error"),
    };

    ErrorBody {
        code: code.to_string(),
        message,
    }
}

impl From<SlackMessage> for Message {
    fn from(value: SlackMessage) -> Self {
        Self {
            subtype: value.subtype,
            user: value.user.or(value.username).or(value.bot_id),
            text: value.text,
            ts: value.ts,
            thread_ts: value.thread_ts,
            reply_count: value.reply_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn top_level_help_documents_output_contract() {
        let mut cmd = Cli::command();
        let help = cmd.render_long_help().to_string();

        assert!(help.contains("Default output is human-readable."));
        assert!(help.contains("Use --json for compact machine-readable JSON."));
    }

    #[test]
    fn resolve_permalink_human_output_is_readable() {
        let rendered = format_resolve_permalink_human(&ResolvedPermalink {
            team_domain: "acme.slack.com".to_string(),
            channel_id: "C123".to_string(),
            message_ts: "123.456".to_string(),
            thread_ts: "123.456".to_string(),
            is_thread_root: true,
            reply_count: Some(4),
        });

        assert!(rendered.contains("slack-query resolve-permalink"));
        assert!(rendered.contains("team: acme.slack.com"));
        assert!(rendered.contains("channel: C123"));
        assert!(rendered.contains("thread_root: true"));
        assert!(rendered.contains("reply_count: 4"));
    }

    #[test]
    fn search_human_output_lists_matches() {
        let rendered = format_search_result_human(&SearchResult {
            query: "forge".to_string(),
            messages: vec![SearchMatch {
                iid: None,
                team: None,
                channel: Some(SearchChannel {
                    id: Some("C123".to_string()),
                    name: Some("eng".to_string()),
                }),
                user: Some("U123".to_string()),
                username: Some("ian".to_string()),
                text: "Ship the forge output cleanup".to_string(),
                ts: "123.456".to_string(),
                permalink: Some("https://acme.slack.com/archives/C123/p123456".to_string()),
            }],
            response_metadata: ResponseMetadata {
                rate_limited: false,
                next_cursor: Some("2".to_string()),
            },
        });

        assert!(rendered.contains("slack-query search: 1 matches for \"forge\""));
        assert!(rendered.contains("rate_limited: false"));
        assert!(rendered.contains("next_cursor: 2"));
        assert!(rendered.contains("1. eng [C123] 123.456 ian"));
        assert!(rendered.contains("Ship the forge output cleanup"));
    }

    #[test]
    fn thread_human_output_renders_messages() {
        let rendered = format_thread_result_human(&ThreadResult {
            channel_id: "C123".to_string(),
            thread_ts: "123.456".to_string(),
            messages: vec![Message {
                subtype: None,
                user: Some("U123".to_string()),
                text: "Hello world".to_string(),
                ts: "123.456".to_string(),
                thread_ts: Some("123.456".to_string()),
                reply_count: Some(1),
            }],
            response_metadata: ResponseMetadata {
                rate_limited: false,
                next_cursor: None,
            },
        });

        assert!(rendered.contains("slack-query read-thread: C123 123.456 (1 messages)"));
        assert!(rendered.contains("[123.456] U123 message"));
        assert!(rendered.contains("Hello world"));
    }

    #[test]
    fn human_error_output_is_not_json() {
        let rendered = format_error_human(
            "slack-query",
            &ErrorBody {
                code: "auth_missing".to_string(),
                message: "missing Slack query auth".to_string(),
            },
        );

        assert!(rendered.starts_with("slack-query error [auth_missing]"));
        assert!(rendered.contains("missing Slack query auth"));
        assert!(!rendered.contains("{\"ok\":false"));
    }
}
