#![cfg(unix)]
mod common;
use common::{CommandExt, backend, invoke};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

#[test]
fn synchronous_jobs_use_wrapper_args_and_participate_in_automatic_cleanup() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    fs::write(&script, "cat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\"}}'\n").unwrap();
    let first = invoke(state.path(), repo.path(), &script, "inspect");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let id = first["id"].as_str().unwrap();
    assert!(Path::new(first["local_log_dir"].as_str().unwrap()).is_dir());
    assert!(fs::read_dir(state.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("run-")
    }));
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute("UPDATE jobs SET updated=0 WHERE id=?1", [id])
        .unwrap();
    let second = invoke(state.path(), repo.path(), &script, "inspect again");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_ne!(second["id"], json!(id));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while Path::new(first["local_log_dir"].as_str().unwrap()).exists()
        || db
            .query_row("SELECT count(*) FROM jobs WHERE id=?1", [id], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
            != 0
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM jobs WHERE id=?1", [id], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn automatic_cleanup_uses_the_submitted_retention_policy() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    fs::write(&script, "cat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\"}}'\n").unwrap();
    let config = state.path().join("policy.toml");
    fs::write(&config, "retention_days = 365\n").unwrap();
    let run = || {
        let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
            .arg("--config")
            .arg(&config)
            .arg("--state-dir")
            .arg(state.path())
            .arg("--codex")
            .arg(backend(state.path()))
            .args(["--agy", "/bin/sh", "--agy-arg"])
            .arg(&script)
            .args(["run", "--cwd"])
            .arg(repo.path())
            .args(["--task", "inspect"])
            .bounded_output();
        assert!(output.status.success());
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let first = run();
    let id = first["id"].as_str().unwrap();
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    db.execute(
        "UPDATE jobs SET updated=?1 WHERE id=?2",
        rusqlite::params![now - 30.0 * 86400.0, id],
    )
    .unwrap();
    let second = run();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while db
        .query_row(
            "SELECT json_extract(data,'$.cleanup_status') FROM jobs WHERE id=?1",
            [second["id"].as_str().unwrap()],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
        != "completed"
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(Path::new(first["local_log_dir"].as_str().unwrap()).exists());
    assert_eq!(
        db.query_row("SELECT count(*) FROM jobs WHERE id=?1", [id], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn doctor_passes_wrapper_prefix_to_version_and_help() {
    let state = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    fs::write(&script, "case \"$1\" in\n--version) echo 'agy 1.2.17';;\n--help) echo '--model --input-format --output-format --disable-slash-commands --dangerously-skip-permissions --print-timeout --log-file --conversation';;\n*) exit 99;;\nesac\n").unwrap();
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
    for name in ["agy", "codex", "agy_headless"] {
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

#[test]
fn automatic_cleanup_keeps_exactly_the_configured_history_count() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    fs::write(&script, "cat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\"}}'\n").unwrap();
    let config = state.path().join("policy.toml");
    fs::write(&config, "max_history_jobs = 10\n").unwrap();
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    for _ in 0..12 {
        let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
            .arg("--config")
            .arg(&config)
            .arg("--state-dir")
            .arg(state.path())
            .arg("--codex")
            .arg(backend(state.path()))
            .args(["--agy", "/bin/sh", "--agy-arg"])
            .arg(&script)
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let status: String = db
                .query_row(
                    "SELECT json_extract(data,'$.cleanup_status') FROM jobs WHERE id=?1",
                    [result["id"].as_str().unwrap()],
                    |r| r.get(0),
                )
                .unwrap();
            if status == "completed" {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    assert_eq!(
        db.query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        10
    );
}

#[test]
fn concurrent_synchronous_results_survive_automatic_retention() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    fs::write(&script, "cat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\"}}'\n").unwrap();
    let config = state.path().join("policy.toml");
    fs::write(&config, "max_history_jobs = 10\n").unwrap();
    let backend = backend(state.path());
    std::thread::scope(|scope| {
        let threads: Vec<_> = (0..20)
            .map(|_| {
                scope.spawn(|| {
                    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
                        .arg("--config")
                        .arg(&config)
                        .arg("--state-dir")
                        .arg(state.path())
                        .arg("--codex")
                        .arg(&backend)
                        .args(["--agy", "/bin/sh", "--agy-arg"])
                        .arg(&script)
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
                    assert_eq!(result["response"], "ok");
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    });
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let (count, pending): (i64,i64) = db.query_row("SELECT count(*),sum(CASE WHEN json_extract(data,'$.cleanup_status')='completed' THEN 0 ELSE 1 END) FROM jobs", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        if pending == 0 {
            assert_eq!(count, 10);
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
