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
fn interrupted_reservations_do_not_consume_retained_history_slots() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let reserved = insert(&jobs, "purging", now + 1.0, None);
    let db = rusqlite::Connection::open(jobs.state.join("jobs.sqlite3")).unwrap();
    db.execute(
        "UPDATE jobs SET data=json_set(data,'$.cleanup_previous_status','succeeded') WHERE id=?1",
        [&reserved],
    )
    .unwrap();
    let history: Vec<_> = (0..10)
        .map(|i| insert(&jobs, "succeeded", now - i as f64, None))
        .collect();
    let newer_sibling = insert(&jobs, "succeeded", now + 2.0, Some(&reserved));
    let expired_sibling = insert(&jobs, "succeeded", 0.0, Some(&reserved));
    let preview = jobs.cleanup(true).unwrap();
    assert_eq!(
        preview["selected_ids"],
        json!([reserved, history[9], expired_sibling])
    );
    let result = jobs.cleanup(false).unwrap();
    assert_eq!(result["deleted_ids"], preview["selected_ids"]);
    assert!(history[..9].iter().all(|id| jobs.get(id).is_ok()));
    assert!(jobs.get(&newer_sibling).is_ok());
}

#[cfg(unix)]
#[test]
fn inaccessible_logs_and_owner_files_report_operational_errors() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let directory_id = insert(&jobs, "succeeded", 0.0, None);
    let file_id = insert(&jobs, "succeeded", 0.0, None);
    let dir = jobs.state.join("jobs").join(&directory_id);
    let file = jobs.state.join("jobs").join(&file_id).join("owner.lock");
    fs::write(&file, "").unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o0)).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o0)).unwrap();
    let result = jobs.cleanup(false).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["errors"].as_array().unwrap().len(), 2);
    for (id, operations) in [
        (&directory_id, &["open_logs", "inspect_owner_lock"][..]),
        (&file_id, &["open_owner_lock"][..]),
    ] {
        let error = result["errors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|error| error["id"] == *id)
            .unwrap();
        assert!(operations.contains(&error["operation"].as_str().unwrap()));
        assert_eq!(
            error["detail"],
            std::io::Error::from_raw_os_error(libc::EACCES).to_string()
        );
        assert_eq!(jobs.get(id).unwrap()["report"], "retained report");
    }
}

#[test]
fn concurrent_cleaners_with_missing_logs_only_report_each_deletion_once() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let ids: Vec<_> = (0..40)
        .map(|_| {
            let id = insert(&jobs, "succeeded", 0.0, None);
            fs::remove_dir_all(jobs.state.join("jobs").join(&id)).unwrap();
            id
        })
        .collect();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let jobs = jobs.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    jobs.cleanup(false).unwrap()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let deleted: Vec<_> = results
        .iter()
        .flat_map(|result| {
            assert_eq!(result["ok"], true);
            result["deleted_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap())
        })
        .collect();
    assert_eq!(deleted.len(), ids.len());
    assert_eq!(
        deleted
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        ids.len()
    );
    assert!(ids.iter().all(|id| jobs.get(id).is_err()));
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

#[cfg(unix)]
#[test]
fn cleanup_refuses_redirected_directories_and_never_follows_links_inside_logs() {
    use std::os::unix::fs::symlink;
    let state = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    fs::write(external.path().join("keep"), "shared agy fixture").unwrap();
    let jobs = jobs(state.path());
    let redirected = insert(&jobs, "succeeded", 0.0, None);
    let path = jobs.state.join("jobs").join(&redirected);
    fs::remove_dir_all(&path).unwrap();
    symlink(external.path(), &path).unwrap();
    let normal = insert(&jobs, "succeeded", 0.0, None);
    symlink(
        external.path(),
        jobs.state.join("jobs").join(&normal).join("linked-profile"),
    )
    .unwrap();
    let cleaned = jobs.cleanup(false).unwrap();
    assert!(has(&cleaned["deleted_ids"], &normal));
    assert!(!has(&cleaned["deleted_ids"], &redirected));
    assert_eq!(
        fs::read_to_string(external.path().join("keep")).unwrap(),
        "shared agy fixture"
    );
    assert!(jobs.get(&redirected).is_ok());
    fs::remove_file(&path).unwrap();
    fs::rename(jobs.state.join("jobs"), state.path().join("original-jobs")).unwrap();
    symlink(external.path(), jobs.state.join("jobs")).unwrap();
    assert!(
        Jobs::new(Config {
            state_dir: Some(state.path().into()),
            ..Config::default()
        })
        .is_err()
    );
    jobs.cleanup(false).unwrap();
    assert!(external.path().join("keep").exists());
}

#[test]
fn owned_terminal_records_do_not_consume_the_eligible_history_limit() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let _leases: Vec<_> = (0..10)
        .map(|_| {
            let id = insert(&jobs, "succeeded", now, None);
            let file =
                fs::File::create(jobs.state.join("jobs").join(id).join("owner.lock")).unwrap();
            file.lock_exclusive().unwrap();
            file
        })
        .collect();
    let available = insert(&jobs, "succeeded", now - 10.0, None);
    assert!(
        jobs.cleanup(false).unwrap()["deleted_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(jobs.get(&available).is_ok());
}

#[cfg(unix)]
#[test]
fn redirected_lock_files_and_replaced_parent_cannot_target_external_files() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let state = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let keep = external.path().join("keep");
    fs::write(&keep, "untouched").unwrap();
    for target in [&keep, &external.path().join("missing")] {
        let id = insert(&jobs, "succeeded", 0.0, None);
        symlink(target, jobs.state.join("jobs").join(&id).join("owner.lock")).unwrap();
        assert!(!has(&jobs.cleanup(true).unwrap()["selected_ids"], &id));
        assert!(!has(&jobs.cleanup(false).unwrap()["deleted_ids"], &id));
        assert!(jobs.get(&id).is_ok());
    }
    let special = insert(&jobs, "succeeded", 0.0, None);
    let lock = jobs.state.join("jobs").join(&special).join("owner.lock");
    let lock = std::ffi::CString::new(lock.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(lock.as_ptr(), 0o600) }, 0);
    assert!(!has(&jobs.cleanup(false).unwrap()["deleted_ids"], &special));
    let normal = insert(&jobs, "succeeded", 0.0, None);
    fs::create_dir(external.path().join(&normal)).unwrap();
    fs::write(external.path().join(&normal).join("keep"), "untouched").unwrap();
    let original = state.path().join("original-jobs");
    fs::rename(state.path().join("jobs"), &original).unwrap();
    symlink(external.path(), state.path().join("jobs")).unwrap();
    assert!(has(&jobs.cleanup(false).unwrap()["deleted_ids"], &normal));
    assert!(!original.join(&normal).exists());
    assert!(external.path().join(&normal).join("keep").exists());
    assert_eq!(fs::read_to_string(keep).unwrap(), "untouched");
}

#[cfg(unix)]
#[test]
fn failed_log_removal_retains_the_report_and_can_be_retried() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    } // Root bypasses this filesystem permission check.
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let id = insert(&jobs, "succeeded", 0.0, None);
    let restricted = jobs.state.join("jobs").join(&id).join("restricted");
    fs::create_dir(&restricted).unwrap();
    fs::write(restricted.join("keep"), "fixture").unwrap();
    fs::set_permissions(&restricted, fs::Permissions::from_mode(0o500)).unwrap();
    let config = state.path().join("config.toml");
    fs::write(&config, "").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--config")
        .arg(config)
        .arg("--state-dir")
        .arg(state.path())
        .arg("cleanup")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["errors"][0]["id"], id);
    assert_eq!(result["errors"][0]["operation"], "remove_logs");
    assert_eq!(result["errors"][0]["record_retained"], true);
    assert_eq!(
        result["errors"][0]["detail"],
        std::io::Error::from_raw_os_error(libc::EACCES).to_string()
    );
    assert_eq!(jobs.get(&id).unwrap()["report"], "retained report");
    fs::set_permissions(restricted, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(has(&jobs.cleanup(false).unwrap()["deleted_ids"], &id));
}

#[test]
fn cleanup_skips_scalar_records_and_recovers_an_interrupted_reservation() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let db = rusqlite::Connection::open(jobs.state.join("jobs.sqlite3")).unwrap();
    for data in ["[]", "null", "123", "\"text\""] {
        let id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO jobs VALUES(?1,?1,'succeeded',0,?2)",
            rusqlite::params![id, data],
        )
        .unwrap();
    }
    let pending = insert(&jobs, "succeeded", 0.0, None);
    db.execute("UPDATE jobs SET status='purging',updated=?1,data=json_set(data,'$.status','purging','$.cleanup_previous_status','succeeded') WHERE id=?2", rusqlite::params![SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64(),pending]).unwrap();
    fs::remove_file(jobs.state.join("jobs").join(&pending).join("report.log")).unwrap();
    let result = jobs.cleanup(false).unwrap();
    assert_eq!(result["skipped"].as_array().unwrap().len(), 4);
    assert!(has(&result["deleted_ids"], &pending));
    assert!(jobs.get(&pending).is_err());
}

#[cfg(unix)]
#[test]
fn jobs_created_after_parent_replacement_retain_their_reports() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    fs::rename(state.path().join("jobs"), state.path().join("old-jobs")).unwrap();
    fs::create_dir(state.path().join("jobs")).unwrap();
    let id = insert(&jobs, "succeeded", 0.0, None);
    let result = jobs.cleanup(false).unwrap();
    assert_eq!(result["skipped"][0]["reason"], "job_root_changed");
    assert!(result["deleted_ids"].as_array().unwrap().is_empty());
    assert_eq!(jobs.get(&id).unwrap()["report"], "retained report");
    assert!(
        state
            .path()
            .join("jobs")
            .join(id)
            .join("report.log")
            .exists()
    );
}

#[test]
fn large_terminal_conversation_uses_the_partial_live_index() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let root = uuid::Uuid::new_v4().to_string();
    let mut db = rusqlite::Connection::open(jobs.state.join("jobs.sqlite3")).unwrap();
    let tx = db.transaction().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    for _ in 0..10000 {
        let id = uuid::Uuid::new_v4().to_string();
        let dir = jobs.state.join("jobs").join(&id);
        let data = json!({"id":id,"root_id":root,"status":"succeeded","local_log_dir":dir});
        tx.execute(
            "INSERT INTO jobs VALUES(?1,?2,'succeeded',?3,?4)",
            rusqlite::params![id, root, now, data.to_string()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    // EXISTS has outer constant-row plan entries; inspect all details for the inner index.
    let plans = db.prepare("EXPLAIN QUERY PLAN SELECT EXISTS(SELECT 1 FROM jobs WHERE root=?1 AND id!=?2 AND status NOT IN ('succeeded','failed','cancelled','timed_out','interrupted','blocked'))").unwrap().query_map([&root,"excluded"], |row|row.get::<_,String>(3)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert!(
        plans.iter().any(|plan| plan.contains("jobs_live_root_id")),
        "{plans:?}"
    );
    assert_eq!(
        jobs.cleanup(true).unwrap()["selected_ids"]
            .as_array()
            .unwrap()
            .len(),
        9990
    );
}
