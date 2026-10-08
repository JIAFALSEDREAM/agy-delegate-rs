#![cfg(unix)]
mod common;
use common::{CommandExt, backend};
use serde_json::Value;
use std::{fs, process::Command};

#[test]
fn doctor_passes_wrapper_prefix_to_version_and_help() {
    let state = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    fs::write(&script, "case \"$1\" in\n--version) echo 'agy 1.2.17';;\n--help) echo '--input-format --output-format --disable-slash-commands --dangerously-skip-permissions --print-timeout --log-file --conversation';;\n*) exit 99;;\nesac\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state.path())
        .arg("--codex")
        .arg(backend(state.path()))
        .args(["--agy", "/bin/sh", "--agy-arg"])
        .arg(&script)
        .arg("doctor")
        .bounded_output();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    for name in ["agy", "agy_headless"] {
        assert_eq!(
            report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|check| check["name"] == name)
                .unwrap()["ok"],
            true
        );
    }
}

#[test]
fn synchronous_ctrl_c_cancels_the_owned_background_task() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    let pidfile = state.path().join("pid");
    fs::write(
        &script,
        format!(
            "cat >/dev/null\necho $$ > '{}'\nexec sleep 30\n",
            pidfile.display()
        ),
    )
    .unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state.path())
        .arg("--codex")
        .arg(backend(state.path()))
        .args(["--agy", "/bin/sh", "--agy-arg"])
        .arg(&script)
        .args(["run", "--cwd"])
        .arg(repo.path())
        .args(["--task", "inspect"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .capture();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !pidfile.exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = common::wait(child);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "cancelled");
    let pid: i32 = fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
}

#[test]
fn ctrl_c_exits_promptly_while_another_connection_keeps_the_database_locked() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    agy_delegate_rs::jobs::Jobs::new(agy_delegate_rs::config::Config {
        state_dir: Some(state.path().into()),
        ..Default::default()
    })
    .unwrap();
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state.path())
        .args(["run", "--cwd"])
        .arg(repo.path())
        .args(["--task", "inspect"])
        .capture();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let start = std::time::Instant::now();
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = common::wait(child);
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interrupted"));
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
