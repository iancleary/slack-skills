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

fn run_slack_cli(args: &[&str]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_slack-query");
    Command::new(bin)
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("run slack-query")
}

#[test]
fn cli_parse_errors_honor_json_flag() {
    let output = run_slack_cli(&["--json", "search"]);
    assert!(!output.status.success());

    let err: Value = serde_json::from_str(String::from_utf8(output.stderr).unwrap().trim())
        .expect("parse error json");
    assert_eq!(err["ok"], false);
    assert_eq!(err["error"]["code"], "invalid_usage");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("the following required arguments were not provided")
    );
}

#[test]
fn cli_parse_errors_default_to_human_output() {
    let output = run_slack_cli(&["search"]);
    assert!(!output.status.success());

    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    assert!(stderr.contains("the following required arguments were not provided"));
    assert!(stderr.contains("Usage: slack-query search <QUERY>"));
    assert!(!stderr.contains("{\"ok\":false"));
}

#[test]
fn cli_version_reports_package_version() {
    let output = run_slack_cli(&["--version"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains(env!("CARGO_PKG_VERSION")));
}
