use crate::{
    config::{Config, private_dir},
    runner,
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
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
        let jobs = Self { config, state };
        jobs.db()?.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY, root TEXT, status TEXT, updated REAL, data TEXT);")?;
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
            let mut query = db.prepare("SELECT id FROM jobs WHERE root=?1 AND status IN ('starting','running','cancelling')")?;
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
            ensure!(!active(&old), "Follow-up requires a stopped task");
            let root = old["root_id"]
                .as_str()
                .context("Missing root ID")?
                .to_owned();
            let count: i64 = tx.query_row("SELECT count(*) FROM jobs WHERE root=?1 AND status IN ('starting','running','cancelling')", [&root], |r|r.get(0))?;
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
