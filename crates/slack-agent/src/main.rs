use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::{
    Client,
    multipart::{Form, Part},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slack_core::{
    ErrorBody, OutputMode, SlackFile, SlackMessage, classify_slack_error_code,
    config_dir_path as shared_config_dir_path, emit_output, format_error_human, normalize_token,
    prepare_config_dir, print_error_json, prompt_for_token, read_thread_messages,
    slack_client as shared_slack_client, slack_get, slack_post_form, slack_post_json,
    write_token_file,
};

#[derive(Parser, Debug)]
#[command(name = "slack-agent", version)]
#[command(
    about = "Assistant-focused Slack CLI with explicit write actions and thread-first defaults"
)]
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
    #[command(subcommand)]
    Thread(ThreadCommand),
    #[command(subcommand)]
    Reply(ReplyCommand),
    #[command(subcommand)]
    Reaction(ReactionCommand),
    #[command(subcommand)]
    File(FileCommand),
    #[command(subcommand)]
    Dm(DmCommand),
    #[command(subcommand)]
    Channel(ChannelCommand),
}

#[derive(Subcommand, Debug)]
enum AuthCommand {
    Login(AuthLoginArgs),
}

#[derive(Subcommand, Debug)]
enum ThreadCommand {
    Read(ThreadReadArgs),
}

#[derive(Subcommand, Debug)]
enum ReplyCommand {
    Send(ReplySendArgs),
}

#[derive(Subcommand, Debug)]
enum ReactionCommand {
    Add(ReactionAddArgs),
}

#[derive(Subcommand, Debug)]
enum FileCommand {
    Upload(FileUploadArgs),
    Info(FileInfoArgs),
}

#[derive(Subcommand, Debug)]
enum DmCommand {
    Open(DmOpenArgs),
    Send(DmSendArgs),
}

#[derive(Subcommand, Debug)]
enum ChannelCommand {
    Join(ChannelJoinArgs),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum TokenKind {
    Bot,
    User,
}

impl TokenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bot => "bot",
            Self::User => "user",
        }
    }
}

#[derive(Args, Debug)]
struct AuthLoginArgs {
    #[arg(long, help = "Slack API token to save instead of prompting")]
    token: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "Persist the token type for docs and diagnostics"
    )]
    token_type: Option<TokenKind>,
    #[arg(long, help = "Overwrite an existing token file")]
    force: bool,
}

#[derive(Args, Debug)]
struct ThreadReadArgs {
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
struct ReplySendArgs {
    #[arg(help = "Slack channel ID, such as C123")]
    channel_id: String,
    #[arg(help = "Thread root timestamp")]
    thread_ts: String,
    #[arg(long, help = "Inline reply body")]
    text: Option<String>,
    #[arg(long, help = "Read the reply body from a file")]
    body_file: Option<PathBuf>,
    #[arg(long, help = "Broadcast the reply to the parent channel")]
    broadcast: bool,
}

#[derive(Args, Debug)]
struct ReactionAddArgs {
    #[arg(help = "Slack channel ID, such as C123")]
    channel_id: String,
    #[arg(help = "Message timestamp")]
    message_ts: String,
    #[arg(help = "Emoji name without surrounding colons")]
    name: String,
}

#[derive(Args, Debug)]
struct FileUploadArgs {
    #[arg(help = "Slack channel ID, such as C123")]
    channel_id: String,
    #[arg(help = "Thread root timestamp; file uploads stay thread-first")]
    thread_ts: String,
    #[arg(help = "Path to the local file to upload")]
    path: PathBuf,
    #[arg(long, help = "Override the file title shown in Slack")]
    title: Option<String>,
    #[arg(long, help = "Optional threaded message text to accompany the upload")]
    initial_comment: Option<String>,
}

#[derive(Args, Debug)]
struct FileInfoArgs {
    #[arg(help = "Slack file ID, such as F123")]
    file_id: String,
}

#[derive(Args, Debug)]
struct DmOpenArgs {
    #[arg(help = "Slack user ID, such as U123")]
    user_id: String,
}

#[derive(Args, Debug)]
struct DmSendArgs {
    #[arg(help = "Slack user ID, such as U123")]
    user_id: String,
    #[arg(long, help = "Inline DM body")]
    text: Option<String>,
    #[arg(long, help = "Read the DM body from a file")]
    body_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ChannelJoinArgs {
    #[arg(help = "Slack channel ID, such as C123")]
    channel_id: String,
}

#[derive(Debug)]
struct AuthState {
    token: String,
}

#[derive(Debug, Serialize)]
struct AuthLoginResult {
    config_dir: String,
    config_file: String,
    token_file: String,
    token_type: Option<String>,
    created: bool,
}

#[derive(Debug, Serialize)]
struct ResponseMetadata {
    rate_limited: bool,
    next_cursor: Option<String>,
}

type Message = SlackMessage;
type FileSummary = SlackFile;

#[derive(Debug, Serialize)]
struct ThreadResult {
    channel_id: String,
    thread_ts: String,
    messages: Vec<Message>,
    response_metadata: ResponseMetadata,
}

#[derive(Debug, Serialize)]
struct ReplySendResult {
    channel_id: String,
    thread_ts: String,
    message_ts: String,
    broadcast: bool,
}

#[derive(Debug, Serialize)]
struct ReactionAddResult {
    channel_id: String,
    message_ts: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct FileUploadResult {
    channel_id: String,
    thread_ts: String,
    file: FileSummary,
}

#[derive(Debug, Serialize)]
struct FileInfoResult {
    file: FileSummary,
}

#[derive(Debug, Serialize)]
struct DmOpenResult {
    user_id: String,
    channel_id: String,
    created: bool,
}

#[derive(Debug, Serialize)]
struct DmSendResult {
    user_id: String,
    channel_id: String,
    message_ts: String,
    thread_ts: String,
}

#[derive(Debug, Serialize)]
struct ChannelJoinResult {
    channel_id: String,
    name: Option<String>,
    is_member: bool,
}

#[derive(Debug, Deserialize)]
struct PostMessageResponse {
    channel: String,
    ts: String,
}

#[derive(Debug, Deserialize)]
struct ConversationsOpenResponse {
    channel: SlackChannel,
    #[serde(default)]
    already_open: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ConversationsJoinResponse {
    channel: SlackChannel,
}

#[derive(Debug, Deserialize)]
struct SlackChannel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    is_member: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct FileUploadUrlResponse {
    #[serde(rename = "file_id")]
    file_id: String,
    #[serde(rename = "upload_url")]
    upload_url: String,
}

#[derive(Debug, Deserialize)]
struct CompleteUploadResponse {
    files: Vec<SlackFile>,
}

#[derive(Debug, Deserialize)]
struct FileInfoApiResponse {
    file: SlackFile,
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
    if let Err(err) = run(cli).await {
        let cli_error = classify_error(&err);
        match output {
            OutputMode::Json => print_error_json(&cli_error),
            OutputMode::Human => eprintln!("{}", format_error_human("slack-agent", &cli_error)),
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
        Command::Thread(ThreadCommand::Read(args)) => {
            let auth = read_auth()?;
            let client = slack_client(&auth.token)?;
            let data = read_thread(&client, &args.channel_id, &args.thread_ts, args.limit).await?;
            emit_output(output, data, format_thread_result_human)?;
        }
        Command::Reply(ReplyCommand::Send(args)) => {
            let auth = read_auth()?;
            let client = slack_client(&auth.token)?;
            let body = read_body_input(args.text, args.body_file.as_deref())?;
            let data = reply_send(
                &client,
                &args.channel_id,
                &args.thread_ts,
                &body,
                args.broadcast,
            )
            .await?;
            emit_output(output, data, format_reply_send_human)?;
        }
        Command::Reaction(ReactionCommand::Add(args)) => {
            let auth = read_auth()?;
            let client = slack_client(&auth.token)?;
            let data =
                reaction_add(&client, &args.channel_id, &args.message_ts, &args.name).await?;
            emit_output(output, data, format_reaction_add_human)?;
        }
        Command::File(FileCommand::Upload(args)) => {
            let auth = read_auth()?;
            let client = slack_client(&auth.token)?;
            let data = file_upload(
                &client,
                &args.channel_id,
                &args.thread_ts,
                &args.path,
                args.title.as_deref(),
                args.initial_comment.as_deref(),
            )
            .await?;
            emit_output(output, data, format_file_upload_human)?;
        }
        Command::File(FileCommand::Info(args)) => {
            let auth = read_auth()?;
            let client = slack_client(&auth.token)?;
            let data = file_info(&client, &args.file_id).await?;
            emit_output(output, data, format_file_info_human)?;
        }
        Command::Dm(DmCommand::Open(args)) => {
            let auth = read_auth()?;
            let client = slack_client(&auth.token)?;
            let data = dm_open(&client, &args.user_id).await?;
            emit_output(output, data, format_dm_open_human)?;
        }
        Command::Dm(DmCommand::Send(args)) => {
            let auth = read_auth()?;
            let client = slack_client(&auth.token)?;
            let body = read_body_input(args.text, args.body_file.as_deref())?;
            let data = dm_send(&client, &args.user_id, &body).await?;
            emit_output(output, data, format_dm_send_human)?;
        }
        Command::Channel(ChannelCommand::Join(args)) => {
            let auth = read_auth()?;
            let client = slack_client(&auth.token)?;
            let data = channel_join(&client, &args.channel_id).await?;
            emit_output(output, data, format_channel_join_human)?;
        }
    }

    Ok(())
}

fn format_auth_login_human(result: &AuthLoginResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "slack-agent auth login: wrote token file");
    let _ = writeln!(out, "config_dir: {}", result.config_dir);
    let _ = writeln!(out, "config_file: {}", result.config_file);
    let _ = writeln!(out, "token_file: {}", result.token_file);
    if let Some(token_type) = result.token_type.as_deref() {
        let _ = writeln!(out, "token_type: {token_type}");
    }
    out.trim_end().to_string()
}

fn format_thread_result_human(result: &ThreadResult) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "slack-agent thread read: {} {} ({} messages)",
        result.channel_id,
        result.thread_ts,
        result.messages.len()
    );
    append_response_metadata(&mut out, &result.response_metadata);
    out.push('\n');
    append_messages(&mut out, &result.messages);
    out.trim_end().to_string()
}

fn format_reply_send_human(result: &ReplySendResult) -> String {
    format!(
        "slack-agent reply send: channel={} thread_ts={} message_ts={} broadcast={}",
        result.channel_id, result.thread_ts, result.message_ts, result.broadcast
    )
}

fn format_reaction_add_human(result: &ReactionAddResult) -> String {
    format!(
        "slack-agent reaction add: channel={} message_ts={} name={}",
        result.channel_id, result.message_ts, result.name
    )
}

fn format_file_upload_human(result: &FileUploadResult) -> String {
    format!(
        "slack-agent file upload: channel={} thread_ts={} file={} ({})",
        result.channel_id,
        result.thread_ts,
        result.file.id,
        result
            .file
            .name
            .as_deref()
            .or(result.file.title.as_deref())
            .unwrap_or("-")
    )
}

fn format_file_info_human(result: &FileInfoResult) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "slack-agent file info: {}", result.file.id);
    if let Some(name) = result.file.name.as_deref() {
        let _ = writeln!(out, "name: {name}");
    }
    if let Some(title) = result.file.title.as_deref() {
        let _ = writeln!(out, "title: {title}");
    }
    if let Some(mimetype) = result.file.mimetype.as_deref() {
        let _ = writeln!(out, "mimetype: {mimetype}");
    }
    if let Some(size) = result.file.size {
        let _ = writeln!(out, "size: {size}");
    }
    if let Some(url) = result.file.url_private_download.as_deref() {
        let _ = writeln!(out, "download_url: {url}");
    }
    out.trim_end().to_string()
}

fn format_dm_open_human(result: &DmOpenResult) -> String {
    format!(
        "slack-agent dm open: user={} channel={} created={}",
        result.user_id, result.channel_id, result.created
    )
}

fn format_dm_send_human(result: &DmSendResult) -> String {
    format!(
        "slack-agent dm send: user={} channel={} message_ts={} thread_ts={}",
        result.user_id, result.channel_id, result.message_ts, result.thread_ts
    )
}

fn format_channel_join_human(result: &ChannelJoinResult) -> String {
    format!(
        "slack-agent channel join: channel={} name={} member={}",
        result.channel_id,
        result.name.as_deref().unwrap_or("-"),
        result.is_member
    )
}

fn append_response_metadata(out: &mut String, metadata: &ResponseMetadata) {
    let _ = writeln!(out, "rate_limited: {}", metadata.rate_limited);
    if let Some(cursor) = metadata.next_cursor.as_deref() {
        let _ = writeln!(out, "next_cursor: {cursor}");
    }
}

fn append_messages(out: &mut String, messages: &[Message]) {
    for message in messages {
        append_message_block(out, message, "");
    }
}

fn append_message_block(out: &mut String, message: &Message, indent: &str) {
    let author = message
        .username
        .as_deref()
        .or(message.user.as_deref())
        .or(message.bot_id.as_deref())
        .unwrap_or("-");
    let _ = writeln!(out, "{indent}{} {}", message.ts, author);
    let preview = preview_human_text(&message.text, 240);
    let _ = writeln!(out, "{indent}  {}", preview);
    if !message.files.is_empty() {
        let files = message
            .files
            .iter()
            .map(|file| {
                file.name
                    .as_deref()
                    .or(file.title.as_deref())
                    .unwrap_or(file.id.as_str())
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "{indent}  files: {files}");
    }
    if !message.reactions.is_empty() {
        let reactions = message
            .reactions
            .iter()
            .map(|reaction| format!("{}x{}", reaction.name, reaction.count))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "{indent}  reactions: {reactions}");
    }
}

fn preview_human_text(text: &str, max_chars: usize) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = cleaned.chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn read_body_input(text: Option<String>, body_file: Option<&Path>) -> Result<String> {
    match (text, body_file) {
        (Some(_), Some(_)) => bail!("provide either --text or --body-file, not both"),
        (None, None) => bail!("provide either --text or --body-file"),
        (Some(text), None) => {
            let text = text.trim().to_string();
            if text.is_empty() {
                bail!("message body cannot be empty");
            }
            Ok(text)
        }
        (None, Some(path)) => {
            let body = fs::read_to_string(path)
                .with_context(|| format!("failed to read body file at {}", path.display()))?;
            let body = body.trim().to_string();
            if body.is_empty() {
                bail!("message body cannot be empty");
            }
            Ok(body)
        }
    }
}

fn read_auth() -> Result<AuthState> {
    Ok(AuthState {
        token: slack_core::read_token(
            "SLACK_AGENT_API_TOKEN",
            "FORGE_SLACK_AGENT_CONFIG_DIR",
            "slack-agent",
            true,
            "missing Slack agent auth; set SLACK_AGENT_API_TOKEN or create ~/.config/forge/slack-agent/config.toml or ~/.config/forge/slack-agent/token",
        )?,
    })
}

fn infer_token_type(token: &str) -> Option<TokenKind> {
    if token.starts_with("xoxb-") {
        Some(TokenKind::Bot)
    } else if token.starts_with("xoxp-") || token.starts_with("xoxc-") || token.starts_with("xoxs-")
    {
        Some(TokenKind::User)
    } else {
        None
    }
}

fn login(args: AuthLoginArgs) -> Result<AuthLoginResult> {
    let config_dir = config_dir_path()?;
    let config_file = config_dir.join("config.toml");
    let token_file_path = config_dir.join("token");

    prepare_config_dir(&config_dir)?;

    if (config_file.exists() || token_file_path.exists()) && !args.force {
        bail!("token file already exists; rerun with --force to overwrite");
    }

    let token = match args.token {
        Some(token) => token,
        None => prompt_for_token("Paste Slack API token: ")?,
    };
    let token = normalize_token(token)?;

    let token_type = args.token_type.or_else(|| infer_token_type(&token));

    let token_file = write_token_file(&config_dir, &token, true)?;

    let token_file_for_config = token_file.display().to_string();
    let mut config_body = format!("token_file = {:?}\n", token_file_for_config);
    if let Some(token_type) = token_type {
        let _ = writeln!(config_body, "token_type = {:?}", token_type.as_str());
    }
    fs::write(&config_file, config_body)
        .with_context(|| format!("failed to write config file at {}", config_file.display()))?;
    slack_core::ensure_owner_only_permissions(&config_file, false)?;

    Ok(AuthLoginResult {
        config_dir: config_dir.display().to_string(),
        config_file: config_file.display().to_string(),
        token_file: token_file.display().to_string(),
        token_type: token_type.map(|value| value.as_str().to_string()),
        created: true,
    })
}

fn config_dir_path() -> Result<PathBuf> {
    shared_config_dir_path("FORGE_SLACK_AGENT_CONFIG_DIR", "slack-agent", true)
}

fn slack_client(token: &str) -> Result<Client> {
    shared_slack_client(token, "slack-skills/slack-agent")
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
        messages: payload.messages,
        response_metadata: ResponseMetadata {
            rate_limited: false,
            next_cursor: payload.next_cursor,
        },
    })
}

async fn reply_send(
    client: &Client,
    channel_id: &str,
    thread_ts: &str,
    body: &str,
    broadcast: bool,
) -> Result<ReplySendResult> {
    let body = json!({
        "channel": channel_id,
        "thread_ts": thread_ts,
        "text": body,
        "reply_broadcast": broadcast,
    });
    let payload: PostMessageResponse = slack_post_json(client, "chat.postMessage", &body).await?;

    Ok(ReplySendResult {
        channel_id: payload.channel,
        thread_ts: thread_ts.to_string(),
        message_ts: payload.ts,
        broadcast,
    })
}

async fn reaction_add(
    client: &Client,
    channel_id: &str,
    message_ts: &str,
    name: &str,
) -> Result<ReactionAddResult> {
    let _: Value = slack_post_form(
        client,
        "reactions.add",
        &[
            ("channel".to_string(), channel_id.to_string()),
            ("timestamp".to_string(), message_ts.to_string()),
            ("name".to_string(), name.to_string()),
        ],
    )
    .await?;

    Ok(ReactionAddResult {
        channel_id: channel_id.to_string(),
        message_ts: message_ts.to_string(),
        name: name.to_string(),
    })
}

async fn file_upload(
    client: &Client,
    channel_id: &str,
    thread_ts: &str,
    path: &Path,
    title: Option<&str>,
    initial_comment: Option<&str>,
) -> Result<FileUploadResult> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read file {}", path.display()))?;
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat file {}", path.display()))?;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("file path must end with a valid UTF-8 filename"))?
        .to_string();
    let title = title.unwrap_or(&filename).to_string();

    let upload_url: FileUploadUrlResponse = slack_post_form(
        client,
        "files.getUploadURLExternal",
        &[
            ("filename".to_string(), filename.clone()),
            ("length".to_string(), metadata.len().to_string()),
        ],
    )
    .await?;

    upload_external_file(&upload_url.upload_url, bytes, &filename).await?;

    let mut body = serde_json::Map::new();
    body.insert(
        "files".to_string(),
        json!([{"id": upload_url.file_id, "title": title}]),
    );
    body.insert("channel_id".to_string(), json!(channel_id));
    body.insert("thread_ts".to_string(), json!(thread_ts));
    if let Some(comment) = initial_comment.filter(|value| !value.trim().is_empty()) {
        body.insert("initial_comment".to_string(), json!(comment));
    }

    let payload: CompleteUploadResponse =
        slack_post_json(client, "files.completeUploadExternal", &Value::Object(body)).await?;
    let file = payload
        .files
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Slack did not return uploaded file metadata"))?;

    Ok(FileUploadResult {
        channel_id: channel_id.to_string(),
        thread_ts: thread_ts.to_string(),
        file,
    })
}

async fn upload_external_file(upload_url: &str, bytes: Vec<u8>, filename: &str) -> Result<()> {
    let upload_client = Client::builder()
        .user_agent("slack-skills/slack-agent")
        .build()
        .context("failed to build file upload client")?;

    let response = upload_client
        .post(upload_url)
        .multipart(Form::new().part(
            "filename",
            Part::bytes(bytes).file_name(filename.to_string()),
        ))
        .send()
        .await
        .context("failed to upload file bytes to Slack")?;

    if !response.status().is_success() {
        bail!("Slack file upload failed with HTTP {}", response.status());
    }

    Ok(())
}

async fn file_info(client: &Client, file_id: &str) -> Result<FileInfoResult> {
    let payload: FileInfoApiResponse = slack_get(
        client,
        "files.info",
        &[("file".to_string(), file_id.to_string())],
    )
    .await?;

    Ok(FileInfoResult { file: payload.file })
}

async fn dm_open(client: &Client, user_id: &str) -> Result<DmOpenResult> {
    let body = json!({ "users": user_id });
    let payload: ConversationsOpenResponse =
        slack_post_json(client, "conversations.open", &body).await?;

    Ok(DmOpenResult {
        user_id: user_id.to_string(),
        channel_id: payload.channel.id,
        created: !payload.already_open.unwrap_or(false),
    })
}

async fn dm_send(client: &Client, user_id: &str, body: &str) -> Result<DmSendResult> {
    let opened = dm_open(client, user_id).await?;
    let body = json!({
        "channel": opened.channel_id,
        "text": body,
    });
    let payload: PostMessageResponse = slack_post_json(client, "chat.postMessage", &body).await?;

    Ok(DmSendResult {
        user_id: user_id.to_string(),
        channel_id: payload.channel,
        message_ts: payload.ts.clone(),
        thread_ts: payload.ts,
    })
}

async fn channel_join(client: &Client, channel_id: &str) -> Result<ChannelJoinResult> {
    let payload: ConversationsJoinResponse = slack_post_form(
        client,
        "conversations.join",
        &[("channel".to_string(), channel_id.to_string())],
    )
    .await?;

    Ok(ChannelJoinResult {
        channel_id: payload.channel.id,
        name: payload.channel.name,
        is_member: payload.channel.is_member.unwrap_or(true),
    })
}

fn classify_error(error: &anyhow::Error) -> ErrorBody {
    let message = error.to_string();
    let code = match message.as_str() {
        msg if msg.contains("missing Slack agent auth") => "auth_missing",
        msg if msg.contains("token file already exists") => "validation_error",
        msg if msg.contains("empty Slack token") => "validation_error",
        msg if msg.contains("provide either --text or --body-file") => "validation_error",
        msg if msg.contains("message body cannot be empty") => "validation_error",
        _ => classify_slack_error_code(&message).unwrap_or("internal_error"),
    };

    ErrorBody {
        code: code.to_string(),
        message,
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
    fn read_body_input_requires_one_source() {
        assert!(read_body_input(None, None).is_err());
        assert!(read_body_input(Some("hi".to_string()), Some(Path::new("body.md"))).is_err());
    }

    #[test]
    fn infer_token_type_handles_bot_and_user_prefixes() {
        assert!(matches!(infer_token_type("xoxb-123"), Some(TokenKind::Bot)));
        assert!(matches!(
            infer_token_type("xoxp-123"),
            Some(TokenKind::User)
        ));
        assert!(infer_token_type("test-token").is_none());
    }
}
