use agy_delegate_rs::{config::Config, jobs::Jobs};
use fs2::FileExt;
use serde_json::json;
use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

fn jobs(state: &Path) -> Jobs {
    Jobs::new(Config {
        state_dir: Some(state.into()),
        ..Config::default()
    })
    .unwrap()
}
fn record(jobs: &Jobs, root: Option<&str>, updated: f64, status: &str, managed: bool) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let root = root.unwrap_or(&id);
    let path = jobs.state.join("jobs").join(&id);
    fs::create_dir(&path).unwrap();
    let job = json!({"id":id,"root_id":root,"status":status,"local_log_dir":path,
        "report":"retained","config":jobs.config,"cwd":jobs.state,"conversation":uuid::Uuid::new_v4().to_string(),"managed_profile":managed});
    rusqlite::Connection::open(jobs.state.join("jobs.sqlite3"))
        .unwrap()
        .execute(
            "INSERT INTO jobs VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![id, root, status, updated, job.to_string()],
        )
        .unwrap();
    id
}
fn profile(jobs: &Jobs, root: &str) -> std::path::PathBuf {
    let path = jobs.state.join("conversations").join(root);
    fs::create_dir_all(path.join("nested")).unwrap();
    fs::write(path.join("nested/history"), "fixture").unwrap();
    path
}
fn age(jobs: &Jobs, id: &str, status: &str, time: f64) {
    rusqlite::Connection::open(jobs.state.join("jobs.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE jobs SET updated=?1,status=?2,data=json_set(data,'$.status',?2) WHERE id=?3",
            rusqlite::params![time, status, id],
        )
        .unwrap();
}

#[test]
fn active_or_referenced_profiles_survive_until_the_last_record_is_purged() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let first = record(&jobs, None, 0.0, "succeeded", true);
    let path = profile(&jobs, &first);
    let recent = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let second = record(&jobs, Some(&first), recent, "running", true);
    assert!(
        jobs.cleanup(false).unwrap()["deleted_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    age(&jobs, &second, "succeeded", recent);
    assert_eq!(jobs.cleanup(false).unwrap()["deleted_ids"], json!([first]));
    assert!(path.join("nested/history").exists());
    age(&jobs, &second, "succeeded", 0.0);
    let preview = jobs.cleanup(true).unwrap();
    assert_eq!(preview["selected_ids"], json!([second]));
    assert!(path.exists());
    let result = jobs.cleanup(false).unwrap();
    assert_eq!(result["deleted_conversation_ids"], json!([first]));
    assert!(!path.exists());
}

#[test]
fn locked_records_and_legacy_or_untracked_profiles_are_preserved() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let locked = record(&jobs, None, 0.0, "succeeded", true);
    let path = profile(&jobs, &locked);
    let lease = fs::File::create(jobs.state.join("jobs").join(&locked).join("owner.lock")).unwrap();
    lease.lock_exclusive().unwrap();
    let legacy = record(&jobs, None, 0.0, "succeeded", false);
    let legacy_path = profile(&jobs, &legacy);
    let unknown = profile(&jobs, &uuid::Uuid::new_v4().to_string());
    let result = jobs.cleanup(false).unwrap();
    assert_eq!(result["deleted_ids"], json!([legacy]));
    for path in [path, legacy_path, unknown] {
        assert!(path.join("nested/history").exists());
    }
}

#[cfg(unix)]
#[test]
fn redirected_profiles_keep_external_files_and_leave_a_retryable_reservation() {
    use std::os::unix::fs::symlink;
    let state = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("personal"), "keep").unwrap();
    let jobs = jobs(state.path());
    let id = record(&jobs, None, 0.0, "succeeded", true);
    let path = jobs.state.join("conversations").join(&id);
    symlink(outside.path(), &path).unwrap();
    let result = jobs.cleanup(false).unwrap();
    assert_eq!(result["ok"], false);
    assert_eq!(result["errors"][0]["operation"], "remove_profile");
    assert_eq!(jobs.get(&id).unwrap()["status"], "purging");
    assert_eq!(jobs.report(&id).unwrap(), "retained");
    assert!(
        jobs.followup(&id, "do not resume partially removed data")
            .is_err()
    );
    assert_eq!(fs::read(outside.path().join("personal")).unwrap(), b"keep");
    fs::remove_file(&path).unwrap();
    let path = profile(&jobs, &id);
    symlink(outside.path(), path.join("link")).unwrap();
    assert_eq!(jobs.cleanup(false).unwrap()["deleted_ids"], json!([id]));
    assert!(!path.exists());
    assert_eq!(fs::read(outside.path().join("personal")).unwrap(), b"keep");
}

#[cfg(unix)]
#[test]
fn a_replaced_profile_parent_cannot_delete_new_contents_or_old_pinned_data() {
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let id = record(&jobs, None, 0.0, "succeeded", true);
    profile(&jobs, &id);
    fs::rename(
        state.path().join("conversations"),
        state.path().join("old-profiles"),
    )
    .unwrap();
    let path = profile(&jobs, &id);
    assert_eq!(jobs.cleanup(false).unwrap()["ok"], false);
    assert!(path.join("nested/history").exists());
    assert!(
        state
            .path()
            .join("old-profiles")
            .join(id)
            .join("nested/history")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn failed_directory_removal_keeps_purging_until_permissions_are_fixed() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let jobs = jobs(state.path());
    let id = record(&jobs, None, 0.0, "succeeded", true);
    let path = profile(&jobs, &id);
    let nested = path.join("nested");
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o500)).unwrap();
    let result = jobs.cleanup(false).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result["errors"][0]["operation"], "remove_profile");
    assert_eq!(jobs.get(&id).unwrap()["status"], "purging");
    assert_eq!(jobs.report(&id).unwrap(), "retained");
    assert!(jobs.followup(&id, "do not resume").is_err());
    assert_eq!(jobs.cleanup(false).unwrap()["deleted_ids"], json!([id]));
    assert!(!path.exists());
}
