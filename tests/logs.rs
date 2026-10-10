#![cfg(unix)]
mod common;
use common::{CommandExt, backend, invoke};
use serde_json::Value;
use std::{fs, path::Path, process::Command};

#[tokio::test]
async fn exiting_leader_does_not_leave_a_descendant_reopening_raw_logs() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    let pidfile = state.path().join("descendant");
    fs::write(&script, format!("while [ \"$1\" != --log-file ]; do shift; done\nshift\ncat >/dev/null\n(while :; do printf private-descendant-fixture > \"$1\"; done) >/dev/null 2>&1 &\necho $! > '{}'\nprintf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"status\":\"SUCCESS\",\"response\":\"ok\"}}}}'\n",pidfile.display())).unwrap();
    let config = agy_delegate_rs::config::Config {
        codex: backend(state.path()),
        agy: "/bin/sh".into(),
        agy_args: vec![script.to_str().unwrap().into()],
        ..Default::default()
    };
    let result = agy_delegate_rs::runner::run(
        &config,
        repo.path(),
        "inspect",
        None,
        state.path(),
        async |_| Ok(()),
    )
    .await;
    let pid: i32 = fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
    // A lingering zombie group may prevent verification: this must retain privately and fail.
    match result {
        Ok(value) => {
            assert_eq!(value["status"], "succeeded");
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        }
        Err(error) => {
            assert!(format!("{error:#}").contains("termination unverified"));
            assert!(state.path().join("tmp/UNVERIFIED_PROCESS").is_file());
        }
    }
}

#[test]
fn dropped_runner_stops_reopening_backend_before_disposing_its_raw_log() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    let pidfile = state.path().join("pid");
    fs::write(&script, format!("while [ \"$1\" != --log-file ]; do shift; done\nshift\ncat >/dev/null\necho $$ > '{}'\nwhile :; do printf private-fixture-value > \"$1\"; done\n",pidfile.display())).unwrap();
    let config = agy_delegate_rs::config::Config {
        codex: backend(state.path()),
        agy: "/bin/sh".into(),
        agy_args: vec![script.to_str().unwrap().into()],
        ..Default::default()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut running = Box::pin(agy_delegate_rs::runner::run(
            &config,
            repo.path(),
            "inspect",
            None,
            state.path(),
            async |_| Ok(()),
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !pidfile.exists() {
            tokio::select! {
                result = &mut running => panic!("Runner stopped early: {result:?}"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {},
            }
            assert!(std::time::Instant::now() < deadline);
        }
        drop(running);
    });
    // Runtime shutdown joins the blocking cleanup even after the async future disappeared.
    drop(runtime);
    let pid: i32 = fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(!state.path().join("tmp/agy.log").exists());
}

#[test]
fn persisted_lossy_stderr_respects_the_configured_byte_limit() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    fs::write(&script, "cat >/dev/null\npython3 -c 'import os; os.write(2, bytes([255]) * 4096)'\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\"}}'\n").unwrap();
    let output = invoke(state.path(), repo.path(), &script, "inspect");
    assert!(
        output.status.success(),
        "stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let bytes =
        fs::read(Path::new(result["local_log_dir"].as_str().unwrap()).join("stderr.log")).unwrap();
    assert!(!bytes.is_empty() && bytes.len() <= 4096);
    assert!(String::from_utf8(bytes).is_ok());
}

#[test]
fn worker_database_failure_stops_sandbox_and_disposes_raw_log() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    let pidfile = state.path().join("pid");
    fs::write(&script, format!("while [ \"$1\" != --log-file ]; do shift; done\nshift\ncat >/dev/null\nprintf '%s' \"$FIXTURE_SECRET\" > \"$1\"\necho $$ > '{}'\nexec sleep 30\n", pidfile.display())).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state.path())
        .arg("--codex")
        .arg(backend(state.path()))
        .args(["--agy", "/bin/sh", "--agy-arg"])
        .arg(&script)
        .env("FIXTURE_SECRET", "private-log-fixture-value")
        .args(["submit", "--cwd"])
        .arg(repo.path())
        .args(["--task", "inspect"])
        .bounded_output();
    assert!(output.status.success());
    let submitted: Value = serde_json::from_slice(&output.stdout).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !pidfile.exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let pid: i32 = fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE jobs").unwrap();
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let log = Path::new(submitted["local_log_dir"].as_str().unwrap()).join("tmp/agy.log");
    // Runner finalization follows process termination; wait for its owner lease to release.
    use fs2::FileExt;
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(log.parent().unwrap().parent().unwrap().join("owner.lock"))
        .unwrap();
    while lease.try_lock_exclusive().is_err() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        !fs::read_to_string(log)
            .unwrap_or_default()
            .contains("private-log-fixture-value")
    );
}

#[test]
fn growing_and_already_completed_agy_logs_are_bounded_and_redacted() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    for wait in [false, true] {
        fs::write(&script, format!("while [ \"$1\" != --log-file ]; do shift; done\nshift\nhead -c 5000 /dev/zero > \"$1\"\ncat >/dev/null\n{}\nprintf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"status\":\"SUCCESS\",\"response\":\"ok\"}}}}'\n", if wait {"sleep 30"} else {""})).unwrap();
        let start = std::time::Instant::now();
        let output = invoke(state.path(), repo.path(), &script, "inspect");
        assert!(!output.status.success());
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["status"], "failed");
        assert!(result["error"].as_str().unwrap().contains("log"));
        let log = Path::new(result["local_log_dir"].as_str().unwrap()).join("tmp/agy.log");
        assert!(fs::metadata(log).unwrap().len() <= 4096);
    }
}

#[test]
fn completed_logs_redact_credentials_and_cannot_redirect_host_writes() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    let victim = state.path().join("outside.txt");
    fs::write(&victim, "keep me").unwrap();
    fs::write(&script, format!("while [ \"$1\" != --log-file ]; do shift; done\nshift\nln -s '{}' \"$1\"\ncat >/dev/null\nprintf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"status\":\"SUCCESS\",\"response\":\"ok\"}}}}'\n", victim.display())).unwrap();
    let output = invoke(state.path(), repo.path(), &script, "inspect");
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
    fs::write(&script, format!("while [ \"$1\" != --log-file ]; do shift; done\nshift\nln '{}' \"$1\"\ncat >/dev/null\nprintf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"status\":\"SUCCESS\",\"response\":\"ok\"}}}}'\n", victim.display())).unwrap();
    let output = invoke(state.path(), repo.path(), &script, "inspect");
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let log = Path::new(result["local_log_dir"].as_str().unwrap()).join("tmp/agy.log");
    fs::write(log, "independent log").unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
    fs::write(&script, "while [ \"$1\" != --log-file ]; do shift; done\nshift\nprintf '%s' \"$FIXTURE_SECRET\" > \"$1\"\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\"}}'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state.path())
        .arg("--codex")
        .arg(backend(state.path()))
        .args(["--agy", "/bin/sh", "--agy-arg"])
        .arg(&script)
        .env("FIXTURE_SECRET", "private-log-fixture-value")
        .args(["run", "--cwd"])
        .arg(repo.path())
        .args(["--task", "inspect"])
        .bounded_output();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        fs::read_to_string(
            Path::new(result["local_log_dir"].as_str().unwrap()).join("tmp/agy.log")
        )
        .unwrap(),
        "[REDACTED]"
    );
}
