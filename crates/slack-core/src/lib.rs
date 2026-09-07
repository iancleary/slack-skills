use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
pub use cli_core::{
    ErrorBody, OutputMode, emit_output, expand_path, format_error_human, print_error_json,
};
use reqwest::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

pub const SLACK_API_BASE: &str = "https://slack.com/api";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SlackMessage {
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub text: String,
    pub ts: String,
    #[serde(default)]
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub reply_count: Option<u32>,
    #[serde(default)]
    pub files: Vec<SlackFile>,
    #[serde(default)]
    pub reactions: Vec<SlackReaction>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SlackFile {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub mimetype: Option<String>,
    #[serde(default)]
    pub filetype: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub url_private: Option<String>,
    #[serde(default)]
    pub url_private_download: Option<String>,
    #[serde(default)]
    pub permalink: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SlackReaction {
    pub name: String,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub users: Vec<String>,
}

#[derive(Debug)]
pub struct SlackMessagesPage {
    pub messages: Vec<SlackMessage>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    token_file: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackErrorResponse {
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackCursor {
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackListResponse<T> {
    messages: Option<Vec<T>>,
    response_metadata: Option<SlackCursor>,
}

pub fn read_token(
    env_var: &str,
    config_dir_env_var: &str,
    config_subdir: &str,
    use_forge_config_dir: bool,
    missing_message: &str,
) -> Result<String> {
    let config_path = config_file_path(config_dir_env_var, config_subdir, use_forge_config_dir)?;
    let mut inline_token = None;
    let mut token_file = None;
    if config_path.exists() {
        let contents = fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read config file at {}", config_path.display()))?;
        let config: ConfigFile = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config file at {}", config_path.display()))?;
        inline_token = config.token;
        token_file = config.token_file;
    }

    cli_core::resolve_token(
        env_var,
        config_dir_env_var,
        config_subdir,
        use_forge_config_dir,
        inline_token.as_deref(),
        token_file.as_deref(),
        missing_message,
    )
}

pub fn config_dir_path(
    config_dir_env_var: &str,
    config_subdir: &str,
    use_forge_config_dir: bool,
) -> Result<PathBuf> {
    cli_core::resolve_config_dir(config_dir_env_var, config_subdir, use_forge_config_dir)
}

pub fn config_file_path(
    config_dir_env_var: &str,
    config_subdir: &str,
    use_forge_config_dir: bool,
) -> Result<PathBuf> {
    cli_core::resolve_config_file(config_dir_env_var, config_subdir, use_forge_config_dir)
}

pub fn prepare_config_dir(config_dir: &Path) -> Result<()> {
    cli_core::prepare_config_dir(config_dir)
}

pub fn write_token_file(config_dir: &Path, token: &str, force: bool) -> Result<PathBuf> {
    cli_core::write_token_file(config_dir, token, force)
}

pub fn prompt_for_token(prompt: &str) -> Result<String> {
    cli_core::prompt_for_secret(prompt, "failed to read Slack token from stdin")
}

pub fn normalize_token(token: String) -> Result<String> {
    cli_core::normalize_secret(token, "empty Slack token")
}

pub fn ensure_owner_only_permissions(path: &Path, is_dir: bool) -> Result<()> {
    cli_core::ensure_owner_only_permissions(path, is_dir)
}

pub fn slack_client(token: &str, user_agent: &str) -> Result<Client> {
    Client::builder()
        .user_agent(user_agent)
        .default_headers({
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {token}")
                    .parse()
                    .context("invalid token for Authorization header")?,
            );
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded; charset=utf-8"
                    .parse()
                    .expect("static header"),
            );
            headers
        })
        .build()
        .context("failed to build HTTP client")
}

pub async fn parse_slack_json_response<T>(response: reqwest::Response) -> Result<T>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response
        .text()
        .await
        .context("failed to read Slack response body")?;

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        if let Some(retry_after) = retry_after {
            bail!(format!("ratelimited: retry after {retry_after} seconds"));
        }
        bail!("ratelimited");
    }

    if !status.is_success() {
        if let Ok(error) = serde_json::from_str::<SlackErrorResponse>(&body) {
            let message = error.error.unwrap_or_else(|| format!("HTTP {status}"));
            bail!(message);
        }
        bail!(format!("Slack API request failed with HTTP {status}"));
    }

    let payload =
        serde_json::from_str::<T>(&body).context("failed to decode Slack response body")?;
    Ok(payload)
}

pub async fn slack_get<T, Q>(client: &Client, method: &str, query: &Q) -> Result<T>
where
    T: DeserializeOwned,
    Q: Serialize + ?Sized,
{
    let response = client
        .get(format!("{SLACK_API_BASE}/{method}"))
        .query(query)
        .send()
        .await
        .with_context(|| format!("failed to call {method}"))?;
    parse_slack_api_response(response, method).await
}

pub async fn slack_get_messages<Q>(
    client: &Client,
    method: &str,
    query: &Q,
) -> Result<SlackMessagesPage>
where
    Q: Serialize + ?Sized,
{
    let payload: SlackListResponse<SlackMessage> = slack_get(client, method, query).await?;
    Ok(SlackMessagesPage {
        messages: payload.messages.unwrap_or_default(),
        next_cursor: payload.response_metadata.and_then(|item| item.next_cursor),
    })
}

pub async fn read_thread_messages(
    client: &Client,
    channel_id: &str,
    thread_ts: &str,
    limit: u32,
) -> Result<SlackMessagesPage> {
    slack_get_messages(
        client,
        "conversations.replies",
        &[
            ("channel".to_string(), channel_id.to_string()),
            ("ts".to_string(), thread_ts.to_string()),
            ("limit".to_string(), limit.to_string()),
            ("inclusive".to_string(), "true".to_string()),
        ],
    )
    .await
}

pub async fn read_history_messages(
    client: &Client,
    channel_id: &str,
    oldest: Option<&str>,
    latest: Option<&str>,
    inclusive: bool,
    limit: u32,
) -> Result<SlackMessagesPage> {
    let mut query = vec![
        ("channel".to_string(), channel_id.to_string()),
        ("inclusive".to_string(), inclusive.to_string()),
        ("limit".to_string(), limit.to_string()),
    ];
    if let Some(oldest) = oldest {
        query.push(("oldest".to_string(), oldest.to_string()));
    }
    if let Some(latest) = latest {
        query.push(("latest".to_string(), latest.to_string()));
    }
    slack_get_messages(client, "conversations.history", &query).await
}

pub async fn slack_post_form<T, F>(client: &Client, method: &str, form: &F) -> Result<T>
where
    T: DeserializeOwned,
    F: Serialize + ?Sized,
{
    let response = client
        .post(format!("{SLACK_API_BASE}/{method}"))
        .form(form)
        .send()
        .await
        .with_context(|| format!("failed to call {method}"))?;
    parse_slack_api_response(response, method).await
}

pub async fn slack_post_json<T, B>(client: &Client, method: &str, body: &B) -> Result<T>
where
    T: DeserializeOwned,
    B: Serialize + ?Sized,
{
    let response = client
        .post(format!("{SLACK_API_BASE}/{method}"))
        .json(body)
        .send()
        .await
        .with_context(|| format!("failed to call {method}"))?;
    parse_slack_api_response(response, method).await
}

async fn parse_slack_api_response<T>(response: reqwest::Response, method: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let value = parse_slack_json_response::<Value>(response).await?;
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("slack_api_error")
            .to_string();
        bail!(message);
    }

    serde_json::from_value(value).with_context(|| format!("failed to decode {method} payload"))
}

pub fn classify_slack_error_code(message: &str) -> Option<&'static str> {
    match message {
        "invalid_auth" | "not_authed" | "token_revoked" | "token_expired" => Some("auth_error"),
        "missing_scope" | "no_permission" | "not_in_channel" => Some("access_denied"),
        "channel_not_found" | "file_not_found" | "user_not_found" => Some("not_found"),
        msg if msg.starts_with("ratelimited") => Some("rate_limited"),
        _ => None,
    }
}
