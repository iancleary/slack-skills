use std::{path::PathBuf, process::Command};

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir parent")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn run_openclaw_slack(args: &[&str]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_slack-agent");
    Command::new(bin)
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("run slack-agent")
}

#[test]
fn cli_parse_errors_honor_json_flag() {
    let output = run_openclaw_slack(&["--json", "thread"]);
    assert!(!output.status.success());

    let err: Value = serde_json::from_str(String::from_utf8(output.stderr).unwrap().trim())
        .expect("parse error json");
    assert_eq!(err["ok"], false);
    assert_eq!(err["error"]["code"], "invalid_usage");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Usage: slack-agent thread [OPTIONS] <COMMAND>")
    );
}

#[test]
fn cli_parse_errors_default_to_human_output() {
    let output = run_openclaw_slack(&["thread"]);
    assert!(!output.status.success());

    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    assert!(stderr.contains("Usage: slack-agent thread [OPTIONS] <COMMAND>"));
    assert!(stderr.contains("Usage: slack-agent thread"));
    assert!(!stderr.contains("{\"ok\":false"));
}

#[test]
fn cli_version_reports_package_version() {
    let output = run_openclaw_slack(&["--version"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("0.1.0"));
}
