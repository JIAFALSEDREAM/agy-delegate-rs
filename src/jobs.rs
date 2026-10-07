use crate::{
    config::{Config, private_dir},
    runner,
};
use anyhow::{Context, Result, ensure};
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::Dir;
use fs2::FileExt;
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::process::Command;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn active(job: &Value) -> bool {
    matches!(
        job["status"].as_str(),
        Some("starting" | "running" | "cancelling")
    )
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), 0) == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::{Foundation::*, System::Threading::*};
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut code = 259;
        let queried = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        queried == 0 || code == 259
    }
}

fn process_info(pid: u32) -> Option<(u64, sysinfo::ProcessStatus)> {
    let mut system = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    system
        .process(pid)
        .map(|process| (process.start_time(), process.status()))
}
fn process_start(pid: u32) -> Option<u64> {
    process_info(pid).map(|info| info.0)
}
fn sandbox_alive(job: &Value) -> bool {
    let Some(pid) = job["sandbox_pid"].as_u64().map(|p| p as u32) else {
        return false;
    };
    if let (Some(expected), Some(actual)) = (job["sandbox_start"].as_u64(), process_start(pid))
        && expected != actual
    {
        return false;
    }
    pid_alive(pid)
}
fn owner(job: &Value) -> Result<fs::File> {
    Ok(fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(
            Path::new(
                job["local_log_dir"]
                    .as_str()
                    .context("Missing job directory")?,
            )
            .join("owner.lock"),
        )?)
}

#[derive(Clone)]
pub struct Jobs {
    pub config: Config,
    pub state: PathBuf,
    job_root: Arc<Dir>,
}

impl Jobs {
    pub fn new(mut config: Config) -> Result<Self> {
        let state = config.state()?;
        config.state_dir = Some(state.clone());
        config.codex = config.executable(
            &config.codex,
            &config.environment()?,
            &std::env::current_dir()?,
        );
        if let Some(path) = &config.agy_env_file {
            config.agy_env_file = Some(path.canonicalize()?);
        }
        let root = Dir::open_ambient_dir(&state, cap_std::ambient_authority())?;
        match root.create_dir("jobs") {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let job_root = Arc::new(root.open_dir_nofollow("jobs")?);
        let jobs = Self {
            config,
            state,
            job_root,
        };
        jobs.db()?.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY, root TEXT, status TEXT, updated REAL, data TEXT); CREATE INDEX IF NOT EXISTS jobs_root_status ON jobs(root,status); CREATE INDEX IF NOT EXISTS jobs_live_root_id ON jobs(root,id) WHERE status NOT IN ('succeeded','failed','cancelled','timed_out','interrupted','blocked');")?;
        Ok(jobs)
    }
    fn db(&self) -> Result<Connection> {
        let db = Connection::open(self.state.join("jobs.sqlite3"))?;
        db.busy_timeout(Duration::from_secs(5))?;
        Ok(db)
    }
    fn load(db: &Connection, id: &str) -> Result<Value> {
        uuid::Uuid::parse_str(id).context("Invalid job ID")?;
        let (data, updated): (String, f64) = db
            .query_row("SELECT data,updated FROM jobs WHERE id=?1", [id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .context("Unknown job ID")?;
        let mut job: Value = serde_json::from_str(&data)?;
        ensure!(job.is_object(), "Invalid job record: expected object");
        job["updated"] = updated.into();
        Ok(job)
    }
    pub fn get(&self, id: &str) -> Result<Value> {
        let db = self.db()?;
        db.busy_timeout(Duration::from_millis(100))?;
        let mut job = Self::load(&db, id)?;
        if active(&job) {
            if now() - job["updated"].as_f64().unwrap_or(0.0) > 20.0 {
                let owner = owner(&job)?;
                let stopped = owner.try_lock_exclusive().is_ok() && !sandbox_alive(&job);
                if stopped {
                    job["status"] = "orphaned".into();
                    job["error"] = "Worker heartbeat expired; task was not restarted".into();
                    db.busy_timeout(Duration::ZERO)?;
                    let updated = db.execute("UPDATE jobs SET status='orphaned',data=?1 WHERE id=?2 AND updated<?3 AND status IN ('starting','running','cancelling')",params![serde_json::to_string(&job)?,id,now()-20.0]);
                    db.busy_timeout(Duration::from_millis(100))?;
                    if let Err(error) = updated
                        && !matches!(
                            error.sqlite_error_code(),
                            Some(
                                rusqlite::ErrorCode::DatabaseBusy
                                    | rusqlite::ErrorCode::DatabaseLocked
                            )
                        )
                    {
                        return Err(error.into());
                    }
                    job = Self::load(&db, id)?;
                } else {
                    job["progress"] = "Worker heartbeat expired; ownership still active".into();
                }
            } else if Path::new(job["local_log_dir"].as_str().unwrap())
                .join("cancel")
                .exists()
            {
                job["status"] = "cancelling".into();
            }
        }
        job.as_object_mut().unwrap().remove("config");
        job.as_object_mut().unwrap().remove("task");
        job.as_object_mut().unwrap().remove("sandbox_pid");
        job.as_object_mut().unwrap().remove("sandbox_start");
        job.as_object_mut()
            .unwrap()
            .remove("cleanup_previous_status");
        if let Some(report) = job["report"].as_str() {
            let limit = self.config.report_chars as usize;
            let truncated = report.chars().count() > limit;
            job["report"] = report.chars().take(limit).collect::<String>().into();
            job["report_truncated"] = truncated.into();
        }
        Ok(job)
    }
    pub fn save(&self, job: &Value) -> Result<()> {
        self.db()?.execute(
            "UPDATE jobs SET status=?1, updated=?2, data=?3 WHERE id=?4",
            params![
                job["status"].as_str(),
                now(),
                serde_json::to_string(job)?,
                job["id"].as_str()
            ],
        )?;
        Ok(())
    }
    pub fn submit(&self, task: &str, cwd: &Path, parent: Option<&str>) -> Result<Value> {
        ensure!(
            !task.trim().is_empty() && task.chars().count() <= 24000,
            "Task must contain 1–24000 characters"
        );
        let cwd = cwd.canonicalize()?;
        ensure!(cwd.is_dir(), "cwd must be a directory");
        if let Some(parent) = parent {
            self.get(parent)?;
            let old = Self::load(&self.db()?, parent)?;
            let db = self.db()?;
            let mut query = db.prepare("SELECT id FROM jobs WHERE root=?1 AND status IN ('starting','running','cancelling','purging')")?;
            let ids = query
                .query_map([old["root_id"].as_str()], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for id in ids {
                self.get(&id)?;
            }
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = uuid::Uuid::new_v4().to_string();
        let (root, conversation) = if let Some(parent) = parent {
            let old = Self::load(&tx, parent)?;
            ensure!(
                !active(&old) && old["status"] != "purging",
                "Follow-up requires a stopped task that is not being cleaned"
            );
            let root = old["root_id"]
                .as_str()
                .context("Missing root ID")?
                .to_owned();
            let count: i64 = tx.query_row("SELECT count(*) FROM jobs WHERE root=?1 AND status IN ('starting','running','cancelling','purging')", [&root], |r|r.get(0))?;
            ensure!(count == 0, "Another task in this conversation is active");
            (
                root,
                old["conversation"]
                    .as_str()
                    .context("Missing conversation ID")?
                    .to_owned()
                    .into(),
            )
        } else {
            (id.clone(), Value::Null)
        };
        let dir = private_dir(&self.state.join("jobs").join(&id))?;
        let tmp = private_dir(&dir.join("tmp"))?;
        self.config.sandbox(&cwd, &tmp)?;
        let cwd_value = serde_json::to_value(&cwd).context("Working directory must be UTF-8")?;
        let dir_value = serde_json::to_value(&dir).context("State directory must be UTF-8")?;
        let config_value =
            serde_json::to_value(&self.config).context("Configured paths must be UTF-8")?;
        let job = json!({"id":id,"root_id":root,"cwd":cwd_value,"status":"starting","created":now(),"updated":now(),"conversation":conversation,"progress":"queued","error":null,"exit_code":null,"local_log_dir":dir_value,"config":config_value,"task":task});
        tx.execute(
            "INSERT INTO jobs VALUES (?1,?2,?3,?4,?5)",
            params![id, root, "starting", now(), serde_json::to_string(&job)?],
        )?;
        tx.commit()?;
        let start = (|| -> Result<()> {
            let mut cmd = Command::new(std::env::current_exe()?);
            cmd.arg("--state-dir")
                .arg(&self.state)
                .args(["worker", "--job-id", &id]);
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(dir.join("worker.log"))?);
            runner::detach(&mut cmd);
            cmd.spawn()?;
            Ok(())
        })();
        if let Err(error) = start {
            let mut job = job;
            job["status"] = "failed".into();
            job["error"] = error.to_string().into();
            self.save(&job)?;
        }
        self.get(&id)
    }
    pub fn followup(&self, id: &str, message: &str) -> Result<Value> {
        self.get(id)?;
        let old = Self::load(&self.db()?, id)?;
        let config: Config = serde_json::from_value(old["config"].clone())?;
        let original = Self {
            config,
            state: self.state.clone(),
            job_root: self.job_root.clone(),
        };
        original.submit(
            message,
            Path::new(old["cwd"].as_str().context("Missing cwd")?),
            Some(id),
        )
    }
    pub fn cancel(&self, id: &str) -> Result<Value> {
        let job = Self::load(&self.db()?, id)?;
        if active(&job) {
            fs::write(
                Path::new(job["local_log_dir"].as_str().unwrap()).join("cancel"),
                "cancel",
            )?;
            #[cfg(unix)]
            {
                let lease = owner(&job)?;
                if lease.try_lock_exclusive().is_ok() {
                    let mut job = Self::load(&self.db()?, id)?;
                    if active(&job) && sandbox_alive(&job) {
                        let pid =
                            job["sandbox_pid"].as_u64().context("Missing sandbox PID")? as u32;
                        let started = job["sandbox_start"]
                            .as_u64()
                            .context("Missing sandbox identity")?;
                        ensure!(
                            process_start(pid) == Some(started),
                            "Cannot verify stopped worker's sandbox identity"
                        );
                        let killed = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
                        ensure!(
                            killed == 0
                                || std::io::Error::last_os_error().raw_os_error()
                                    == Some(libc::ESRCH),
                            "Cannot stop orphan sandbox"
                        );
                        let deadline = std::time::Instant::now() + Duration::from_secs(2);
                        while process_info(pid).is_some_and(|(time, status)| {
                            time == started && status != sysinfo::ProcessStatus::Zombie
                        }) {
                            ensure!(
                                std::time::Instant::now() < deadline,
                                "Orphan sandbox cleanup timeout"
                            );
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        job["status"] = "cancelled".into();
                        job["progress"] = "finished".into();
                        self.save(&job)?;
                    }
                }
            }
        }
        self.get(id)
    }
    pub fn cleanup(&self, dry_run: bool) -> Result<Value> {
        self.cleanup_using(dry_run, |child, _| child.map_or(Ok(()), Self::remove_logs))
    }
    fn remove_logs(child: Dir) -> std::io::Result<()> {
        // Reopen from the verified root, with no-follow at every component.
        // Keep descriptor and call-stack use constant, even for deeply nested tmp trees.
        let mut pending = vec![(PathBuf::new(), false)];
        while let Some((path, visited)) = pending.pop() {
            if visited && path.as_os_str().is_empty() {
                continue;
            }
            let mut dir = child.try_clone()?;
            for component in path.components() {
                dir = dir.open_dir_nofollow(component)?;
            }
            if visited {
                dir.remove_open_dir()?;
            } else {
                pending.push((path.clone(), true));
                for entry in dir.entries()? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        pending.push((path.join(entry.file_name()), false));
                    } else {
                        dir.remove_file(entry.file_name())?;
                    }
                }
            }
        }
        // Final removal is nonrecursive: a concurrent substitution cannot lose contents.
        child.remove_open_dir()
    }
    fn cleanup_using(
        &self,
        dry_run: bool,
        mut remove: impl FnMut(Option<Dir>, &str) -> std::io::Result<()>,
    ) -> Result<Value> {
        self.config.validate()?;
        // Keep cleaner ownership outside the tree whose owner.lock is removed.
        // Never unlink this stable lock: other cleaners may already have it open.
        let _cleanup_lease = if dry_run {
            None
        } else {
            let mut options = cap_std::fs::OpenOptions::new();
            options
                .read(true)
                .write(true)
                .create(true)
                .follow(FollowSymlinks::No);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NONBLOCK);
            }
            let file = self.job_root.open_with(".cleanup.lock", &options)?;
            ensure!(file.metadata()?.is_file(), "Unsafe cleanup lock");
            let file = file.into_std();
            match file.try_lock_exclusive() {
                Ok(()) => Some(file),
                Err(error)
                    if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
                {
                    return Ok(
                        json!({"ok":true,"dry_run":false,"selected_ids":[],"deleted_ids":[],"errors":[],"skipped":[{"reason":"cleanup_active"}]}),
                    );
                }
                Err(error) => return Err(error.into()),
            }
        };
        let directory = self.state.join("jobs");
        let mut db = self.db()?;
        let rows = {
            let mut query =
                db.prepare("SELECT id,root,status,updated FROM jobs ORDER BY (status='purging') DESC,updated DESC,id")?;
            query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, f64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let terminal = |status: &str| {
            [
                "succeeded",
                "failed",
                "cancelled",
                "timed_out",
                "interrupted",
                "blocked",
            ]
            .contains(&status)
        };
        let protected: std::collections::BTreeSet<_> = rows
            .iter()
            .filter(|row| !terminal(&row.2) && row.2 != "purging")
            .map(|row| row.1.as_str())
            .collect();
        let cutoff = now() - self.config.retention_days as f64 * 86400.0;
        let mut rank = 0_u64;
        let mut preview_purged = std::collections::BTreeSet::new();
        let (mut selected, mut deleted, mut skipped, mut errors) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (id, root, status, updated) in &rows {
            if (!terminal(status) && status != "purging") || protected.contains(root.as_str()) {
                continue;
            }
            let job = match Self::load(&db, id) {
                Ok(job) => job,
                Err(error) => {
                    match error.downcast_ref::<rusqlite::Error>() {
                        Some(rusqlite::Error::QueryReturnedNoRows) => {}
                        Some(database_error) => errors.push(
                            json!({"id":id,"operation":"read_record","detail":database_error.to_string()}),
                        ),
                        None => skipped.push(json!({"id":id,"reason":"invalid_record"})),
                    }
                    continue;
                }
            };
            let path = directory.join(id);
            if uuid::Uuid::parse_str(id).is_err()
                || job["local_log_dir"].as_str().map(Path::new) != Some(path.as_path())
                || job["id"].as_str() != Some(id.as_str())
                || job["status"].as_str() != Some(status.as_str())
                || job["root_id"].as_str() != Some(root.as_str())
            {
                skipped.push(json!({"id":id,"reason":"unverified_log_directory"}));
                continue;
            }
            if status == "purging"
                && !job["cleanup_previous_status"]
                    .as_str()
                    .is_some_and(terminal)
            {
                skipped.push(json!({"id":id,"reason":"invalid_cleanup_reservation"}));
                continue;
            }
            let child = match self.job_root.symlink_metadata(id) {
                Ok(meta) if meta.is_dir() => match self.job_root.open_dir_nofollow(id) {
                    Ok(child) => Some(child),
                    Err(error) => {
                        errors.push(
                            json!({"id":id,"operation":"open_logs","detail":error.to_string()}),
                        );
                        continue;
                    }
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let current = Dir::open_ambient_dir(&self.state, cap_std::ambient_authority())
                        .and_then(|state| state.open_dir_nofollow("jobs"))
                        .and_then(|root| root.dir_metadata());
                    let expected = self.job_root.dir_metadata()?;
                    let current = match current {
                        Ok(current) => current,
                        Err(error) => {
                            errors.push(json!({"id":id,"operation":"verify_job_root","detail":error.to_string()}));
                            continue;
                        }
                    };
                    if current.dev() != expected.dev() || current.ino() != expected.ino() {
                        skipped.push(json!({"id":id,"reason":"job_root_changed"}));
                        continue;
                    }
                    None
                }
                Ok(_) => {
                    skipped.push(json!({"id":id,"reason":"unsafe_log_directory"}));
                    continue;
                }
                Err(error) => {
                    errors.push(
                        json!({"id":id,"operation":"inspect_logs","detail":error.to_string()}),
                    );
                    continue;
                }
            };
            let mut lease = if let Some(child) = &child {
                match child.symlink_metadata("owner.lock") {
                    Ok(meta) if meta.is_file() => {
                        let mut options = cap_std::fs::OpenOptions::new();
                        options.read(true).write(true).follow(FollowSymlinks::No);
                        #[cfg(unix)]
                        {
                            use cap_std::fs::OpenOptionsExt;
                            options.custom_flags(libc::O_NONBLOCK);
                        }
                        match child.open_with("owner.lock", &options) {
                            Ok(file) => match file.metadata() {
                                Ok(meta) if meta.is_file() => Some(file.into_std()),
                                Ok(_) => {
                                    skipped.push(json!({"id":id,"reason":"unsafe_owner_lock"}));
                                    continue;
                                }
                                Err(error) => {
                                    errors.push(json!({"id":id,"operation":"inspect_owner_lock","detail":error.to_string()}));
                                    continue;
                                }
                            },
                            Err(error) => {
                                errors.push(json!({"id":id,"operation":"open_owner_lock","detail":error.to_string()}));
                                continue;
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Ok(_) => {
                        skipped.push(json!({"id":id,"reason":"unsafe_owner_lock"}));
                        continue;
                    }
                    Err(error) => {
                        errors.push(json!({"id":id,"operation":"inspect_owner_lock","detail":error.to_string()}));
                        continue;
                    }
                }
            } else {
                None
            };
            if let Some(file) = &lease
                && let Err(error) = file.try_lock_exclusive()
            {
                if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                    skipped.push(json!({"id":id,"reason":"ownership_active"}));
                } else {
                    errors
                        .push(json!({"id":id,"operation":"lock_owner","detail":error.to_string()}));
                }
                continue;
            }
            if sandbox_alive(&job) {
                skipped.push(json!({"id":id,"reason":"ownership_active"}));
                continue;
            }
            let blocked = {
                let mut query = db.prepare("SELECT id FROM jobs WHERE root=?1 AND id!=?2 AND status NOT IN ('succeeded','failed','cancelled','timed_out','interrupted','blocked')")?;
                query
                    .query_map(params![root, id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .iter()
                    .any(|id| !preview_purged.contains(id))
            };
            if blocked {
                continue;
            }
            let chosen =
                status == "purging" || *updated < cutoff || rank >= self.config.max_history_jobs;
            if status != "purging" {
                rank += 1;
            }
            if !chosen {
                continue;
            }
            selected.push(id.clone());
            if dry_run {
                if status == "purging" {
                    preview_purged.insert(id.clone());
                }
                continue;
            }
            if lease.is_none()
                && let Some(child) = &child
            {
                let mut options = cap_std::fs::OpenOptions::new();
                options
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .follow(FollowSymlinks::No);
                match child.open_with("owner.lock", &options) {
                    Ok(file) => {
                        let file = file.into_std();
                        if let Err(error) = file.try_lock_exclusive() {
                            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                                skipped.push(json!({"id":id,"reason":"ownership_active"}));
                            } else {
                                errors.push(json!({"id":id,"operation":"lock_owner","detail":error.to_string()}));
                            }
                            continue;
                        }
                        lease = Some(file);
                    }
                    Err(error) => {
                        errors.push(json!({"id":id,"operation":"create_owner_lock","detail":error.to_string()}));
                        continue;
                    }
                }
            }
            // Reserve under a short writer transaction. All filesystem deletion is outside it.
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = match Self::load(&tx, id) {
                Ok(current) => current,
                Err(error)
                    if matches!(
                        error.downcast_ref::<rusqlite::Error>(),
                        Some(rusqlite::Error::QueryReturnedNoRows)
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let blocked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE root=?1 AND id!=?2 AND status NOT IN ('succeeded','failed','cancelled','timed_out','interrupted','blocked'))", params![root,id], |row|row.get(0))?;
            if blocked || current["status"] != job["status"] || current["root_id"] != job["root_id"]
            {
                skipped.push(json!({"id":id,"reason":"changed_or_active_conversation"}));
                continue;
            }
            if status != "purging" {
                tx.execute("UPDATE jobs SET status='purging',data=json_set(data,'$.status','purging','$.cleanup_previous_status',?1) WHERE id=?2", params![status,id])?;
            }
            tx.commit()?;
            // The stable cleanup lock and purging status now protect ownership.
            // Close the per-job file before deletion so Windows can remove its directory.
            drop(lease);
            if let Err(error) = remove(child, id) {
                db.execute("UPDATE jobs SET status=json_extract(data,'$.cleanup_previous_status'),data=json_remove(json_set(data,'$.status',json_extract(data,'$.cleanup_previous_status')),'$.cleanup_previous_status') WHERE id=?1 AND status='purging'", [id])?;
                errors.push(json!({"id":id,"operation":"remove_logs","detail":error.to_string(),"record_retained":true}));
                continue;
            }
            if db.execute("DELETE FROM jobs WHERE id=?1 AND status='purging'", [id])? == 1 {
                deleted.push(id.clone());
            }
        }
        Ok(
            json!({"ok":errors.is_empty(),"dry_run":dry_run,"selected_ids":selected,"deleted_ids":deleted,"skipped":skipped,"errors":errors}),
        )
    }
    pub fn list(&self, limit: u64) -> Result<Value> {
        ensure!((1..=20).contains(&limit), "Limit must be between 1 and 20");
        let db = self.db()?;
        let mut query = db
            .prepare("SELECT id FROM jobs ORDER BY json_extract(data,'$.created') DESC LIMIT ?1")?;
        let ids = query
            .query_map([limit], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(json!({"result":ids.iter().map(|id|self.get(id)).collect::<Result<Vec<_>>>()?}))
    }
    pub async fn wait(&self, id: &str, seconds: u64) -> Result<Value> {
        ensure!(seconds <= 55, "Wait must be between 0 and 55 seconds");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        loop {
            let jobs = self.clone();
            let key = id.to_owned();
            let job = tokio::task::spawn_blocking(move || jobs.get(&key)).await??;
            if !active(&job) || tokio::time::Instant::now() >= deadline {
                return Ok(job);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub async fn worker(&self, id: &str) -> Result<()> {
        let mut job = Self::load(&self.db()?, id)?;
        let owner = owner(&job)?;
        owner
            .try_lock_exclusive()
            .context("Worker already owns this job")?;
        let claimed = self.db()?.execute("UPDATE jobs SET status='running',data=json_set(data,'$.status','running'),updated=?1 WHERE id=?2 AND status='starting'",params![now(),id])?;
        ensure!(claimed == 1, "Job was already claimed or is stopped");
        job = Self::load(&self.db()?, id)?;
        let config: Config = serde_json::from_value(job["config"].clone())?;
        let cwd = PathBuf::from(job["cwd"].as_str().context("Missing cwd")?);
        let dir = PathBuf::from(
            job["local_log_dir"]
                .as_str()
                .context("Missing job directory")?,
        );
        let task = job["task"].as_str().context("Missing task")?.to_owned();
        let conversation = job["conversation"].as_str().map(str::to_owned);
        job["status"] = "running".into();
        job["worker_pid"] = std::process::id().into();
        self.save(&job)?;
        let (updates, mut receiver) = tokio::sync::watch::channel(job.clone());
        let (acknowledged, mut ownership) = tokio::sync::watch::channel(false);
        let storage = self.clone();
        let mut persistence = tokio::spawn(async move {
            let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    changed = receiver.changed() => if changed.is_err() { break; },
                    _ = heartbeat.tick() => {}
                }
                let snapshot = receiver.borrow_and_update().clone();
                let owns_sandbox = snapshot["sandbox_pid"].is_number();
                let storage = storage.clone();
                tokio::task::spawn_blocking(move || storage.save(&snapshot)).await??;
                if owns_sandbox {
                    acknowledged.send_replace(true);
                }
            }
            Ok::<(), anyhow::Error>(())
        });
        let mut persistence_finished = false;
        let mut result = {
            let future = runner::run(
                &config,
                &cwd,
                &task,
                conversation.as_deref(),
                &dir,
                async |event| {
                    let started = event["event"] == "sandbox_started";
                    if event["conversation_id"].is_string() {
                        job["conversation"] = event["conversation_id"].clone();
                    }
                    if event["event"] == "sandbox_started" {
                        job["sandbox_pid"] = event["pid"].clone();
                        job["sandbox_start"] = event["pid"]
                            .as_u64()
                            .and_then(|p| process_start(p as u32))
                            .into();
                    }
                    if event["event"] == "step_update" {
                        let step = &event["step_update"];
                        job["progress"] = step["tool_name"]
                            .as_str()
                            .or(step["step_type"].as_str())
                            .unwrap_or("step")
                            .into();
                    }
                    updates.send_replace(job.clone());
                    if started {
                        while !*ownership.borrow_and_update() {
                            ownership
                                .changed()
                                .await
                                .context("Sandbox ownership was not persisted")?;
                        }
                    }
                    Ok(())
                },
            );
            tokio::pin!(future);
            tokio::select! {
                value = &mut future => value,
                stored = &mut persistence => {
                    persistence_finished = true;
                    Err(match stored {
                        Err(error) => error.into(),
                        Ok(Err(error)) => error,
                        Ok(Ok(())) => anyhow::anyhow!("Task persistence stopped unexpectedly"),
                    })
                }
            }
        };
        drop(updates);
        if !persistence_finished
            && let Err(error) = persistence
                .await
                .context("Task persistence panicked")
                .and_then(|value| value)
        {
            result = Err(error);
        }
        match result {
            Ok(value) => {
                job["status"] = value["status"].clone();
                job["error"] = value["error"].clone();
                job["exit_code"] = value["exit_code"].clone();
                if value["conversation_id"].is_string() {
                    job["conversation"] = value["conversation_id"].clone();
                }
                if value["response"].is_string() {
                    job["report"] = value["response"].clone();
                }
                if value["usage"].is_object() {
                    job["usage"] = value["usage"].clone();
                    job["usage_scope"] = "conversation_cumulative".into();
                }
            }
            Err(error) => {
                job["status"] = "failed".into();
                job["error"] = error.to_string().into();
            }
        }
        job["progress"] = "finished".into();
        self.save(&job)
    }
}

#[cfg(all(test, unix))]
mod cleanup_concurrency {
    use super::*;
    #[test]
    fn removal_uses_the_verified_handle_after_job_directory_substitution() {
        let state = tempfile::tempdir().unwrap();
        let jobs = Jobs::new(Config {
            state_dir: Some(state.path().into()),
            ..Config::default()
        })
        .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let dir = jobs.state.join("jobs").join(&id);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("old.log"), "expired").unwrap();
        let data = json!({"id":id,"root_id":id,"status":"succeeded","local_log_dir":dir});
        jobs.db()
            .unwrap()
            .execute(
                "INSERT INTO jobs VALUES(?1,?1,'succeeded',0,?2)",
                params![id, data.to_string()],
            )
            .unwrap();
        let moved = jobs.state.join("jobs").join("renamed");
        let result = jobs
            .cleanup_using(false, |child, _| {
                // Removing the per-job lock must not let a second cleaner take over.
                fs::remove_file(dir.join("owner.lock"))?;
                let competing = jobs.cleanup(false).unwrap();
                assert_eq!(competing["skipped"][0]["reason"], "cleanup_active");
                assert_eq!(jobs.get(&id).unwrap()["status"], "purging");
                fs::rename(&dir, &moved)?;
                fs::create_dir(&dir)?;
                fs::write(dir.join("replacement.log"), "must survive")?;
                Jobs::remove_logs(child.unwrap())
            })
            .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(
            fs::read_to_string(dir.join("replacement.log")).unwrap(),
            "must survive"
        );
        assert!(!moved.exists());
        assert!(jobs.get(&id).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_log_removal_preserves_a_running_worker_and_blocks_followup() {
        use std::os::unix::fs::PermissionsExt;
        let state = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let backend = state.path().join("backend");
        fs::write(&backend, "#!/bin/sh\ncat >/dev/null\nsleep 7\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"complete\"}}'\n").unwrap();
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o700)).unwrap();
        let jobs = Jobs::new(Config {
            codex: backend,
            state_dir: Some(state.path().into()),
            timeout: 20,
            ..Config::default()
        })
        .unwrap();
        let old = uuid::Uuid::new_v4().to_string();
        let running = uuid::Uuid::new_v4().to_string();
        for (id, status, updated) in [(&old, "succeeded", 0.0), (&running, "starting", now())] {
            let dir = jobs.state.join("jobs").join(id);
            fs::create_dir_all(&dir).unwrap();
            let data = json!({"id":id,"root_id":id,"status":status,"created":updated,"config":jobs.config,"task":"inspect","cwd":repo.path(),"local_log_dir":dir,"conversation":"fixture"});
            jobs.db()
                .unwrap()
                .execute(
                    "INSERT INTO jobs VALUES(?1,?1,?2,?3,?4)",
                    params![id, status, updated, data.to_string()],
                )
                .unwrap();
        }
        let worker_jobs = jobs.clone();
        let worker_id = running.clone();
        let worker = tokio::task::spawn_blocking(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(worker_jobs.worker(&worker_id))
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while jobs.get(&running).unwrap()["status"] != "running" {
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let before = jobs.get(&running).unwrap()["updated"].as_f64().unwrap();
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let cleaner = jobs.clone();
        let cleanup = tokio::task::spawn_blocking(move || {
            let mut ready = Some(ready);
            cleaner.cleanup_using(false, |child, _| {
                ready.take().unwrap().send(()).unwrap();
                std::thread::sleep(Duration::from_secs(6));
                Jobs::remove_logs(child.unwrap())
            })
        });
        waiting.await.unwrap();
        assert_eq!(jobs.get(&old).unwrap()["status"], "purging");
        assert!(jobs.followup(&old, "continue").is_err());
        assert_eq!(cleanup.await.unwrap().unwrap()["ok"], true);
        assert!(jobs.get(&running).unwrap()["updated"].as_f64().unwrap() > before + 4.0);
        tokio::time::timeout(Duration::from_secs(12), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(jobs.get(&running).unwrap()["status"], "succeeded");
    }
}
