use std::{
    env,
    fmt::Write as _,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
}

impl OutputMode {
    pub fn from_json_flag(json: bool) -> Self {
        if json { Self::Json } else { Self::Human }
    }
}

#[derive(Debug, Serialize)]
pub struct Envelope<T>
where
    T: Serialize,
{
    pub ok: bool,
    pub data: T,
}

#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    ok: bool,
    error: ErrorBody,
}

#[derive(Debug, Serialize, Clone)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

pub fn print_json<T>(value: &T) -> Result<()>
where
    T: Serialize,
{
    println!(
        "{}",
        serde_json::to_string(value).context("failed to render JSON output")?
    );
    Ok(())
}

pub fn emit_output<T, F>(mode: OutputMode, data: T, human: F) -> Result<()>
where
    T: Serialize,
    F: FnOnce(&T) -> String,
{
    match mode {
        OutputMode::Json => print_json(&Envelope { ok: true, data }),
        OutputMode::Human => {
            print_human_text(&human(&data));
            Ok(())
        }
    }
}

pub fn print_human_text(text: &str) {
    if text.ends_with('\n') {
        print!("{text}");
    } else {
        println!("{text}");
    }
}

pub fn format_error_human(tool_name: &str, error: &ErrorBody) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{tool_name} error [{}]", error.code);
    for line in error.message.lines() {
        let _ = writeln!(out, "{line}");
    }
    out.trim_end().to_string()
}

pub fn print_error_json(error: &ErrorBody) {
    eprintln!(
        "{}",
        serde_json::to_string(&ErrorEnvelope {
            ok: false,
            error: error.clone(),
        })
        .expect("error envelopes should serialize")
    );
}

pub fn resolve_config_dir(
    config_dir_env_var: &str,
    config_subdir: &str,
    use_forge_config_dir: bool,
) -> Result<PathBuf> {
    if let Ok(path) = env::var(config_dir_env_var) {
        return Ok(expand_path(&path));
    }
    if use_forge_config_dir && let Ok(path) = env::var("FORGE_CONFIG_DIR") {
        return Ok(expand_path(&path).join(config_subdir));
    }
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("forge").join(config_subdir));
    }

    let home = env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("forge")
        .join(config_subdir))
}

pub fn resolve_config_file(
    config_dir_env_var: &str,
    config_subdir: &str,
    use_forge_config_dir: bool,
) -> Result<PathBuf> {
    Ok(
        resolve_config_dir(config_dir_env_var, config_subdir, use_forge_config_dir)?
            .join("config.toml"),
    )
}

pub fn prepare_config_dir(config_dir: &Path) -> Result<()> {
    fs::create_dir_all(config_dir)
        .with_context(|| format!("failed to create config dir at {}", config_dir.display()))?;
    ensure_owner_only_permissions(config_dir, true)?;
    Ok(())
}

pub fn write_token_file(config_dir: &Path, token: &str, force: bool) -> Result<PathBuf> {
    let token_file = config_dir.join("token");
    if token_file.exists() && !force {
        bail!("token file already exists; rerun with --force to overwrite");
    }
    fs::write(&token_file, format!("{token}\n"))
        .with_context(|| format!("failed to write token file at {}", token_file.display()))?;
    ensure_owner_only_permissions(&token_file, false)?;
    Ok(token_file)
}

pub fn prompt_for_secret(prompt: &str, read_error: &str) -> Result<String> {
    let mut stdout = io::stdout();
    write!(stdout, "{prompt}").context("failed to write auth prompt")?;
    stdout.flush().context("failed to flush auth prompt")?;

    let mut buffer = String::new();
    io::stdin()
        .read_line(&mut buffer)
        .with_context(|| read_error.to_string())?;
    Ok(buffer)
}

pub fn normalize_secret(secret: String, empty_message: &str) -> Result<String> {
    let secret = secret.trim().to_string();
    if secret.is_empty() {
        bail!(empty_message.to_string());
    }
    Ok(secret)
}

pub fn resolve_token(
    env_var: &str,
    config_dir_env_var: &str,
    config_subdir: &str,
    use_forge_config_dir: bool,
    inline_token: Option<&str>,
    token_file: Option<&str>,
    missing_message: &str,
) -> Result<String> {
    if let Ok(token) = env::var(env_var) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    if let Some(token) = inline_token {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    if let Some(token_file) = token_file {
        let token_path = expand_path(token_file);
        let token = fs::read_to_string(&token_path)
            .with_context(|| format!("failed to read token file at {}", token_path.display()))?;
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token_path =
        resolve_config_dir(config_dir_env_var, config_subdir, use_forge_config_dir)?.join("token");
    if token_path.exists() {
        let token = fs::read_to_string(&token_path)
            .with_context(|| format!("failed to read token file at {}", token_path.display()))?;
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    bail!(missing_message.to_string())
}

pub fn ensure_owner_only_permissions(path: &Path, is_dir: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = if is_dir { 0o700 } else { 0o600 };
        fs::set_permissions(path, PermissionsExt::from_mode(mode))
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        let _ = is_dir;
    }

    Ok(())
}

pub fn expand_path(path: &str) -> PathBuf {
    if path == "~"
        && let Ok(home) = env::var("HOME")
    {
        return PathBuf::from(home);
    }

    if let Some(stripped) = path.strip_prefix("~/")
        && let Ok(home) = env::var("HOME")
    {
        return PathBuf::from(home).join(stripped);
    }

    PathBuf::from(path)
}
