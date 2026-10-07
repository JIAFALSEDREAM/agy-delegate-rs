use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
};

fn cli(state: &Path, backend: &Path, timeout: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state)
        .arg("--codex")
        .arg(backend)
        .args(["--timeout", timeout])
        .args(args)
        .output()
        .unwrap()
}
fn json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn failed_backend_persists_without_unsandboxed_fallback() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let missing = state.path().join("missing-codex");
    let job = json(cli(
        state.path(),
        &missing,
        "2",
        &[
            "submit",
            "--cwd",
            repo.path().to_str().unwrap(),
            "--task",
            "inspect",
        ],
    ));
    let id = job["id"].as_str().unwrap();
    let stopped = json(cli(
        state.path(),
        &missing,
        "2",
        &["wait", id, "--seconds", "5"],
    ));
    assert_eq!(stopped["status"], "failed");
    assert!(
        stopped["error"]
            .as_str()
            .unwrap()
            .contains("unsandboxed execution is refused")
    );
    assert!(stopped.get("config").is_none());
    assert!(
        !cli(state.path(), &missing, "2", &["worker", "--job-id", id])
            .status
            .success()
    );
    assert_eq!(
        json(cli(state.path(), &missing, "2", &["status", id]))["status"],
        "failed"
    );
    assert!(
        !cli(state.path(), &missing, "2", &["status", "../other"])
            .status
            .success()
    );
    assert!(
        !cli(state.path(), &missing, "2", &["followup", id, "continue"])
            .status
            .success()
    );
}
