#![cfg(unix)]
mod common;
use agy_delegate_rs::{config::Config, jobs::Jobs};
use common::{CommandExt, backend};
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

#[tokio::test]
async fn shutdown_cancels_live_jobs_and_preserves_history_and_user_files() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let script = state.path().join("agy.sh");
    let pidfile = state.path().join("pid");
    fs::write(
        &script,
        format!(
            "cat >/dev/null\necho $$ > {}\nexec sleep 30\n",
            pidfile.display()
        ),
    )
    .unwrap();
    let config = state.path().join("config.toml");
    fs::write(&config, "").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .env("HOME", state.path())
        .env("XDG_CONFIG_HOME", state.path().join("config"))
        .env("XDG_DATA_HOME", state.path().join("data"))
        .arg("--config")
        .arg(&config)
        .arg("--state-dir")
        .arg(state.path())
        .arg("--codex")
        .arg(backend(state.path()))
        .args(["--agy", "/bin/sh", "--agy-arg"])
        .arg(&script)
        .args(["submit", "--cwd"])
        .arg(repo.path())
        .args(["--task", "inspect"])
        .bounded_output();
    assert!(output.status.success());
    let submitted: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pidfile.exists() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let jobs = Jobs::new(Config {
        state_dir: Some(state.path().into()),
        ..Config::default()
    })
    .unwrap();
    fs::write(repo.path().join("personal"), "keep").unwrap();
    jobs.stop_all().await.unwrap();
    assert_eq!(
        jobs.get(submitted["id"].as_str().unwrap()).unwrap()["status"],
        "cancelled"
    );
    assert_eq!(
        fs::read_to_string(repo.path().join("personal")).unwrap(),
        "keep"
    );
    let pid: i32 = fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    jobs.stop_all().await.unwrap();
}

#[tokio::test]
async fn shutdown_refuses_a_terminal_job_with_unverifiable_live_sandbox() {
    let state = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(Config {
        state_dir: Some(state.path().into()),
        ..Config::default()
    })
    .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let dir = state.path().join("jobs").join(&id);
    fs::create_dir(&dir).unwrap();
    // A live ordinary PID without process-group ownership cannot be verified safely.
    let job = serde_json::json!({"id":id,"root_id":id,"status":"failed","local_log_dir":dir,"sandbox_pid":unsafe { libc::getpgrp() }});
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO jobs VALUES(?1,?1,?2,0,?3)",
        rusqlite::params![id, "failed", job.to_string()],
    )
    .unwrap();
    assert!(jobs.stop_all().await.is_err());
    fs::remove_dir_all(&dir).unwrap();
    assert!(jobs.stop_all().await.is_err());
}

#[tokio::test]
async fn shutdown_barrier_blocks_submission_until_integration_removal_finishes() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(Config {
        state_dir: Some(state.path().into()),
        ..Config::default()
    })
    .unwrap();
    let shutdown = jobs.shutdown().unwrap();
    shutdown.stop_all().await.unwrap();
    assert!(
        jobs.submit("inspect", repo.path(), None)
            .unwrap_err()
            .to_string()
            .contains("shutting down")
    );
    assert!(jobs.shutdown().is_err());
    let id = uuid::Uuid::new_v4().to_string();
    let directory = state.path().join("jobs").join(&id);
    fs::create_dir(&directory).unwrap();
    let record =
        serde_json::json!({"id":id,"root_id":id,"status":"starting","local_log_dir":directory});
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO jobs VALUES(?1,?1,'starting',0,?2)",
        rusqlite::params![id, record.to_string()],
    )
    .unwrap();
    assert!(
        jobs.worker(&id)
            .await
            .unwrap_err()
            .to_string()
            .contains("shutting down")
    );
    shutdown.stop_all().await.unwrap();
    assert_eq!(jobs.get(&id).unwrap()["status"], "cancelled");
    drop(shutdown);
    assert!(jobs.worker(&id).await.is_err());
    let entry = state.path().join("jobs").join("renamed-task");
    fs::create_dir(&entry).unwrap();
    assert!(jobs.stop_all().await.is_err());
    fs::remove_dir(entry).unwrap();
    jobs.stop_all().await.unwrap();
}

#[tokio::test]
async fn abandoned_shutdown_keeps_its_barrier_until_the_blocking_pass_finishes() {
    let state = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(Config {
        state_dir: Some(state.path().into()),
        ..Config::default()
    })
    .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let directory = state.path().join("jobs").join(&id);
    fs::create_dir(&directory).unwrap();
    let owner = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(directory.join("owner.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&owner).unwrap();
    let record =
        serde_json::json!({"id":id,"root_id":id,"status":"failed","local_log_dir":directory});
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO jobs VALUES(?1,?1,'failed',0,?2)",
        rusqlite::params![id, record.to_string()],
    )
    .unwrap();
    let mut stopping = Box::pin(jobs.stop_all());
    tokio::select! {
        result = &mut stopping => panic!("Shutdown finished while owner held: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(100)) => {},
    }
    drop(stopping);
    assert!(jobs.shutdown().is_err());
    fs2::FileExt::unlock(&owner).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while jobs.shutdown().is_err() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn shutdown_waits_for_retention_before_inspecting_task_directories() {
    let state = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(Config {
        state_dir: Some(state.path().into()),
        ..Config::default()
    })
    .unwrap();
    let path = state.path().join("jobs/.cleanup.lock");
    let cleaner = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&cleaner).unwrap();
    let mut stopping = Box::pin(jobs.stop_all());
    tokio::select! {
        result = &mut stopping => panic!("Shutdown bypassed active cleaner: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(100)) => {},
    }
    drop(cleaner);
    stopping.await.unwrap();
}
