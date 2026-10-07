use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
    time::Duration,
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

#[test]
fn malformed_records_error_and_reserved_history_rejects_followup() {
    use agy_delegate_rs::{config::Config, jobs::Jobs};
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(Config {
        state_dir: Some(state.path().into()),
        ..Config::default()
    })
    .unwrap();
    let db = rusqlite::Connection::open(jobs.state.join("jobs.sqlite3")).unwrap();
    for data in ["[]", "null", "123", "\"text\""] {
        let id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO jobs VALUES(?1,?1,'succeeded',0,?2)",
            rusqlite::params![id, data],
        )
        .unwrap();
        assert!(jobs.get(&id).is_err());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let data = serde_json::json!({"id":id,"root_id":id,"status":"purging","local_log_dir":state.path().join("jobs").join(&id),"cwd":repo.path(),"config":jobs.config,"conversation":"fixture","cleanup_previous_status":"succeeded"});
    db.execute(
        "INSERT INTO jobs VALUES(?1,?1,'purging',0,?2)",
        rusqlite::params![id, data.to_string()],
    )
    .unwrap();
    let error = jobs.followup(&id, "continue").unwrap_err().to_string();
    assert!(error.contains("being cleaned"));
    assert!(
        jobs.get(&id)
            .unwrap()
            .get("cleanup_previous_status")
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn wait_timeout_keeps_task_alive_and_cancel_stops_it() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let backend = state.path().join("slow-backend");
    std::fs::write(&backend, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let job = json(cli(
        state.path(),
        &backend,
        "5",
        &[
            "submit",
            "--cwd",
            repo.path().to_str().unwrap(),
            "--task",
            "inspect",
        ],
    ));
    let id = job["id"].as_str().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while json(cli(state.path(), &backend, "5", &["status", id]))["status"] != "running" {
        assert!(
            std::time::Instant::now() < deadline,
            "worker startup timeout"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let pending = json(cli(
        state.path(),
        &backend,
        "5",
        &["wait", id, "--seconds", "0"],
    ));
    assert_eq!(pending["status"], "running");
    assert!(
        !cli(state.path(), &backend, "5", &["worker", "--job-id", id])
            .status
            .success()
    );
    json(cli(state.path(), &backend, "5", &["cancel", id]));
    assert_eq!(
        json(cli(
            state.path(),
            &backend,
            "5",
            &["wait", id, "--seconds", "5"]
        ))["status"],
        "cancelled"
    );
}

#[cfg(unix)]
#[test]
fn followup_keeps_parent_backend_and_reconciles_stopped_orphan() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let backend = state.path().join("backend");
    std::fs::write(&backend,"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"conversation_id\":\"fixture-conversation\",\"status\":\"SUCCESS\",\"response\":\"ok\"}}'\n").unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let job = json(cli(
        state.path(),
        &backend,
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
    assert_eq!(
        json(cli(
            state.path(),
            &backend,
            "2",
            &["wait", id, "--seconds", "5"]
        ))["status"],
        "succeeded"
    );
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute("UPDATE jobs SET status='running',updated=0,data=json_set(data,'$.status','running') WHERE id=?1",[id]).unwrap();
    let missing = state.path().join("wrong-backend");
    let follow = json(cli(
        state.path(),
        &missing,
        "2",
        &["followup", id, "continue"],
    ));
    assert_eq!(follow["root_id"], job["root_id"]);
    assert_eq!(
        json(cli(
            state.path(),
            &missing,
            "2",
            &["wait", follow["id"].as_str().unwrap(), "--seconds", "5"]
        ))["status"],
        "succeeded"
    );
}

#[cfg(unix)]
#[test]
fn task_deadline_is_distinct_from_wait_timeout() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let backend = state.path().join("slow-backend");
    std::fs::write(&backend, "#!/bin/sh\nsleep 30\n").unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let job = json(cli(
        state.path(),
        &backend,
        "1",
        &[
            "submit",
            "--cwd",
            repo.path().to_str().unwrap(),
            "--task",
            "inspect",
        ],
    ));
    assert_eq!(
        json(cli(
            state.path(),
            &backend,
            "1",
            &["wait", job["id"].as_str().unwrap(), "--seconds", "10"]
        ))["status"],
        "timed_out"
    );
}

#[tokio::test]
async fn stale_heartbeat_preserves_owned_jobs_and_rejects_reused_process_identity() {
    use agy_delegate_rs::{config::Config, jobs::Jobs};
    use fs2::FileExt;
    let state = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(Config {
        codex: "codex".into(),
        agy: "agy".into(),
        agy_env_file: None,
        model: None,
        timeout: 2,
        state_dir: Some(state.path().to_owned()),
        ..Config::default()
    })
    .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let dir = state.path().join(&id);
    std::fs::create_dir(&dir).unwrap();
    let owner = std::fs::File::create(dir.join("owner.lock")).unwrap();
    owner.lock_exclusive().unwrap();
    let data = serde_json::json!({"id":id,"root_id":id,"status":"running","local_log_dir":dir,"created":0,"sandbox_pid":std::process::id(),"sandbox_start":0});
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO jobs VALUES(?1,?1,'running',0,?2)",
        rusqlite::params![id, data.to_string()],
    )
    .unwrap();
    assert_eq!(jobs.get(&id).unwrap()["status"], "running");
    drop(owner);
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    assert_eq!(jobs.wait(&id, 0).await.unwrap()["status"], "running");
    assert!(started.elapsed() < Duration::from_secs(1));
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(jobs.get(&id).unwrap()["status"], "orphaned");
    assert_eq!(
        db.query_row("SELECT status FROM jobs WHERE id=?1", [id], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "orphaned"
    );
}

#[cfg(unix)]
#[test]
fn database_contention_does_not_delay_sandbox_cancellation() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let backend = state.path().join("backend");
    let pidfile = state.path().join("pid");
    std::fs::write(
        &backend,
        format!(
            "#!/bin/sh\necho $$ > '{}.tmp'\nmv '{}.tmp' '{}'\nsleep 30\n",
            pidfile.display(),
            pidfile.display(),
            pidfile.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let job = json(cli(
        state.path(),
        &backend,
        "30",
        &[
            "submit",
            "--cwd",
            repo.path().to_str().unwrap(),
            "--task",
            "inspect",
        ],
    ));
    let id = job["id"].as_str().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !pidfile.exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid: i32 = std::fs::read_to_string(pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    json(cli(state.path(), &backend, "30", &["cancel", id]));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "DB lock delayed cancellation"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    db.execute_batch("COMMIT").unwrap();
    assert_eq!(
        json(cli(
            state.path(),
            &backend,
            "30",
            &["wait", id, "--seconds", "5"]
        ))["status"],
        "cancelled"
    );
}

#[cfg(unix)]
#[test]
fn cancellation_recovers_after_worker_is_killed() {
    use fs2::FileExt;
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let backend = state.path().join("backend");
    std::fs::write(&backend,"#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"event\":\"init\",\"conversation_id\":\"fixture-conversation\"}'\nsleep 30\n").unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let job = json(cli(
        state.path(),
        &backend,
        "30",
        &[
            "submit",
            "--cwd",
            repo.path().to_str().unwrap(),
            "--task",
            "inspect",
        ],
    ));
    let id = job["id"].as_str().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let running = loop {
        let snapshot = json(cli(state.path(), &backend, "30", &["status", id]));
        if snapshot["conversation"] == "fixture-conversation" {
            break snapshot;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    let data: String = db
        .query_row("SELECT data FROM jobs WHERE id=?1", [id], |row| row.get(0))
        .unwrap();
    let data: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(
        unsafe { libc::kill(data["worker_pid"].as_u64().unwrap() as i32, libc::SIGKILL) },
        0
    );
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(Path::new(running["local_log_dir"].as_str().unwrap()).join("owner.lock"))
        .unwrap();
    while lease.try_lock_exclusive().is_err() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(lease);
    let cancelled = json(cli(state.path(), &backend, "30", &["cancel", id]));
    assert_eq!(cancelled["status"], "cancelled");
    let follow = json(cli(
        state.path(),
        &backend,
        "30",
        &["followup", id, "continue"],
    ));
    assert_eq!(follow["root_id"], job["root_id"]);
    json(cli(
        state.path(),
        &backend,
        "30",
        &["cancel", follow["id"].as_str().unwrap()],
    ));
    assert_eq!(
        json(cli(
            state.path(),
            &backend,
            "30",
            &["wait", follow["id"].as_str().unwrap(), "--seconds", "5"]
        ))["status"],
        "cancelled"
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_fail_without_panicking() {
    use std::os::unix::ffi::OsStringExt;
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let repo = root
        .path()
        .join(std::ffi::OsString::from_vec(b"repo-\xff".to_vec()));
    std::fs::create_dir(&repo).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state.path())
        .arg("submit")
        .arg("--cwd")
        .arg(repo)
        .args(["--task", "inspect"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("Working directory must be UTF-8"));
    assert!(!error.contains("panicked"));
}
