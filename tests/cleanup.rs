use agy_delegate_rs::{config::Config, jobs::Jobs};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

fn jobs(state: &Path) -> Jobs {
    Jobs::new(Config {
        state_dir: Some(state.into()),
        retention_days: 1,
        max_history_jobs: 10,
        ..Config::default()
    })
    .unwrap()
}
fn insert(jobs: &Jobs, status: &str, updated: f64, root: Option<&str>) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let root = root.unwrap_or(&id);
    let dir = jobs.state.join("jobs").join(&id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("report.log"), "private fixture").unwrap();
    let data = json!({"id":id,"root_id":root,"status":status,"local_log_dir":dir,"created":updated,"report":"retained report"});
    let db = rusqlite::Connection::open(jobs.state.join("jobs.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO jobs VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![id, root, status, updated, data.to_string()],
    )
    .unwrap();
    id
}
fn has(values: &Value, id: &str) -> bool {
    values.as_array().unwrap().iter().any(|value| value == id)
}

#[test]
fn cleanup_preview_and_age_filter_preserve_owners_orphans_and_active_conversations() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let old = insert(&jobs, "succeeded", 0.0, None);
    let locked = insert(&jobs, "failed", 0.0, None);
    let lease = fs::File::create(jobs.state.join("jobs").join(&locked).join("owner.lock")).unwrap();
    lease.lock_exclusive().unwrap();
    let active = insert(&jobs, "running", 0.0, None);
    let orphan = insert(&jobs, "orphaned", 0.0, None);
    let parent = insert(&jobs, "succeeded", 0.0, None);
    let child = insert(&jobs, "starting", 0.0, Some(&parent));
    let missing_logs = insert(&jobs, "failed", 0.0, None);
    fs::remove_dir_all(jobs.state.join("jobs").join(&missing_logs)).unwrap();
    let preview = jobs.cleanup(true).unwrap();
    assert!(has(&preview["selected_ids"], &old));
    assert!(has(&preview["selected_ids"], &missing_logs));
    assert!(preview["deleted_ids"].as_array().unwrap().is_empty());
    assert!(
        jobs.state
            .join("jobs")
            .join(&old)
            .join("report.log")
            .exists()
    );
    assert!(
        !jobs
            .state
            .join("jobs")
            .join(&old)
            .join("owner.lock")
            .exists()
    );
    assert!(jobs.get(&old).is_ok());
    let cleaned = jobs.cleanup(false).unwrap();
    assert_eq!(cleaned["ok"], true);
    assert_eq!(cleaned["deleted_ids"].as_array().unwrap().len(), 2);
    assert!(jobs.get(&old).is_err());
    assert!(jobs.get(&missing_logs).is_err());
    assert!(!jobs.state.join("jobs").join(&old).exists());
    for id in [&locked, &active, &orphan, &parent, &child] {
        assert!(jobs.state.join("jobs").join(id).join("report.log").exists());
    }
}

#[test]
fn cleanup_keeps_the_newest_history_and_cli_uses_configured_limits() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let ids: Vec<_> = (0..12)
        .map(|i| insert(&jobs, "succeeded", now - i as f64, None))
        .collect();
    let config = state.path().join("config.toml");
    fs::write(&config, "max_history_jobs = 10\n").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--config")
        .arg(config)
        .arg("--state-dir")
        .arg(state.path())
        .args(["cleanup", "--dry-run"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let preview: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(has(&preview["selected_ids"], &ids[10]));
    assert!(has(&preview["selected_ids"], &ids[11]));
    assert_eq!(preview["selected_ids"].as_array().unwrap().len(), 2);
    for id in &ids {
        assert!(jobs.get(id).is_ok());
        assert!(jobs.state.join("jobs").join(id).join("report.log").exists());
    }
    let cleaned = jobs.cleanup(false).unwrap();
    assert_eq!(cleaned["deleted_ids"].as_array().unwrap().len(), 2);
    for id in &ids[..10] {
        assert!(jobs.get(id).is_ok());
    }
    let directory = jobs.state.join("jobs").join(&ids[0]);
    assert!(directory.exists());
}
