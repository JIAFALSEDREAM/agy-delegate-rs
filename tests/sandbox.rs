use agy_delegate_rs::{
    config::{Config, private_dir},
    runner::classify,
};
use serde_json::json;
use std::process::Stdio;

fn config() -> Config {
    Config {
        codex: "codex".into(),
        agy: "agy".into(),
        agy_env_file: None,
        model: None,
        timeout: 5,
        state_dir: None,
    }
}

#[test]
fn zero_exit_with_no_report_is_failure() {
    for malformed in [json!("bad"), json!([]), json!(true), json!(1)] {
        assert_eq!(classify(0, Some(malformed))["status"], "failed");
    }
    assert_eq!(classify(0, None)["status"], "failed");
    assert_eq!(
        classify(0, Some(json!({"status":"SUCCESS","response":""})))["status"],
        "failed"
    );
    assert_eq!(
        classify(0, Some(json!({"status":"WAITING"})))["status"],
        "blocked"
    );
    assert_eq!(
        classify(1, Some(json!({"status":"SUCCESS","response":"fake"})))["status"],
        "failed"
    );
}

#[test]
fn decoded_secrets_with_json_escapes_are_redacted() {
    let secret = "fixture\\quoted\"key\nline".to_owned();
    let mut event: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&json!({"nested":[secret]})).unwrap()).unwrap();
    agy_delegate_rs::runner::redact_value(&mut event, &[secret]);
    assert_eq!(event["nested"][0], "[REDACTED]");
    let mut keys = json!({"fixture-secret-key":{"fixture-secret-key":"fixture-secret-key"}});
    agy_delegate_rs::runner::redact_value(&mut keys, &["fixture-secret-key".into()]);
    assert_eq!(keys["[REDACTED]"]["[REDACTED]"], "[REDACTED]");
    let mut numeric = json!({"usage":{"key":12345678}});
    agy_delegate_rs::runner::redact_value(&mut numeric, &["12345678".into()]);
    assert_eq!(numeric["usage"]["key"], "[REDACTED]");
}

#[cfg(unix)]
#[test]
fn executable_lookup_uses_effective_path() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("fixture-backend");
    std::fs::write(&executable, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env =
        std::collections::BTreeMap::from([("PATH".into(), dir.path().as_os_str().to_owned())]);
    assert_eq!(
        config().executable(std::path::Path::new("fixture-backend"), &env, dir.path()),
        executable
    );
}

#[test]
fn writable_directory_cannot_be_in_project() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = private_dir(&dir.path().join("tmp")).unwrap();
    assert!(config().sandbox(dir.path(), &tmp).is_err());
}

#[test]
fn fixture_child() {
    let Ok(file) = std::env::var("AGY_SANDBOX_FIXTURE") else {
        return;
    };
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
    assert!(std::fs::write(file, "changed").is_err());
}

#[tokio::test]
#[ignore = "requires installed Codex CLI and a usable OS sandbox"]
async fn native_sandbox_preserves_fixture() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let tmp = private_dir(&state.path().join("tmp")).unwrap();
    let file = workspace.path().join("source.txt");
    std::fs::write(&file, "original").unwrap();
    let mut cmd = config().sandbox(workspace.path(), &tmp).unwrap();
    cmd.arg(std::env::current_exe().unwrap())
        .args(["--exact", "fixture_child", "--nocapture"])
        .env("AGY_SANDBOX_FIXTURE", &file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = cmd.output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), "original");
}
