#![cfg(unix)]
mod common;
use common::{CommandExt, backend, invoke};
use serde_json::Value;
use std::{fs, path::Path, process::Command};

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
    fs::write(&script, "cat >/dev/null\nhead -c 4096 /dev/zero | tr '\\000' '\\377' >&2\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\"}}'\n").unwrap();
    let output = invoke(state.path(), repo.path(), &script, "inspect");
    assert!(output.status.success());
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
