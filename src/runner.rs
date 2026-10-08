use crate::config::{Config, private_dir};
use anyhow::{Context, Result, ensure};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::Dir;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

pub fn detach(command: &mut Command) {
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    command.creation_flags(0x00000200 | 0x08000000);
}
struct OwnedChild {
    process: Option<Box<dyn ChildWrapper>>,
    raw_log: Option<RawLog>,
    group_id: Option<u32>,
}
impl OwnedChild {
    fn process(&mut self) -> &mut Box<dyn ChildWrapper> {
        self.process.as_mut().unwrap()
    }
    async fn stop(&mut self) -> Result<RawLog> {
        let process = self.process.take().context("Missing sandbox process")?;
        let raw_log = self.raw_log.take().context("Missing raw log ownership")?;
        let group_id = self.group_id;
        tokio::task::spawn_blocking(move || stop_owned(process, raw_log, group_id)).await?
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let Some(mut process) = self.process.take() else {
            return;
        };
        let _ = process.start_kill();
        if let Some(raw_log) = self.raw_log.take() {
            let group_id = self.group_id;
            let cleanup = move || {
                // Blocking tasks survive async cancellation and are joined at runtime shutdown.
                let _ = stop_owned(process, raw_log, group_id);
            };
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(cleanup);
            } else {
                cleanup();
            }
        }
    }
}
fn stop_owned(
    mut process: Box<dyn ChildWrapper>,
    mut raw_log: RawLog,
    _group_id: Option<u32>,
) -> Result<RawLog> {
    let stopped = (|| -> Result<()> {
        let killed = process.start_kill();
        #[cfg(unix)]
        let killed = killed.or_else(|error| {
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        });
        killed.context("Cannot stop owned sandbox tree")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let reaped = matches!(process.try_wait(), Ok(Some(_)));
            #[cfg(windows)]
            if reaped {
                return Ok(());
            }
            #[cfg(unix)]
            if let Some(pid) = _group_id {
                let gone = unsafe { libc::kill(-(pid as i32), 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                if gone && reaped {
                    return Ok(());
                }
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Sandbox shutdown timeout"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if let Err(error) = stopped {
        // Never pretend raw data was sanitized when process termination could not be verified.
        raw_log.1 = false;
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        if let Ok(mut marker) = raw_log.0.open_with("UNVERIFIED_PROCESS", &options) {
            use std::io::Write;
            let _ = marker.write_all(
                b"Raw log retained privately; verify process termination before cleanup.\n",
            );
        }
        return Err(error.context("Process termination unverified; raw log retained privately"));
    }
    Ok(raw_log)
}
struct PipeTask(tokio::task::JoinHandle<std::io::Result<Vec<u8>>>);
impl Drop for PipeTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct RawLog(Arc<Dir>, bool);
impl Drop for RawLog {
    fn drop(&mut self) {
        if self.1 {
            let _ = self.0.remove_file("agy.log");
        }
    }
}

async fn notify(
    future: impl Future<Output = Result<()>>,
    cancel: &Path,
    stop: &AtomicBool,
) -> Result<bool> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result.map(|()|true),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if cancel.exists() || stop.load(Ordering::Relaxed) { return Ok(false); }
            }
        }
    }
}

fn permission_denied(error: &Value) -> bool {
    match error {
        Value::String(text) => stderr_denial(text),
        Value::Object(object) => {
            ["code", "status", "type"].iter().any(|key| {
                object
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|text| {
                        matches!(
                            text.to_lowercase().replace(['_', '-'], " ").as_str(),
                            "soft denied" | "permission denied" | "eacces"
                        )
                    })
            }) || ["message", "reason"]
                .iter()
                .any(|key| object.get(*key).is_some_and(permission_denied))
        }
        Value::Array(values) => values.iter().any(permission_denied),
        _ => false,
    }
}
fn stderr_denial(text: &str) -> bool {
    let text = text.to_lowercase().replace(['_', '-'], " ");
    text.split(['\n', '\r', '\0']).any(|line| {
        let line = line.trim();
        let diagnostic = line.strip_prefix("error: ").unwrap_or(line);
        let terms = [
            "soft denied",
            "permission denied",
            "denied permission",
            "permission request denied",
        ];
        let direct = |text: &str| {
            terms.iter().any(|term| {
                text.strip_prefix(term).is_some_and(|rest| {
                    rest.is_empty()
                        || rest.starts_with([':', ','])
                        || rest.starts_with(" (os error ")
                })
            })
        };
        direct(diagnostic)
            || [
                "cat:", "rg:", "grep:", "find:", "sed:", "head:", "tail:", "ls:", "stat:", "open:",
                "eacces:",
            ]
            .iter()
            .any(|prefix| {
                diagnostic.strip_prefix(prefix).is_some_and(|rest| {
                    direct(rest.trim())
                        || rest
                            .rsplit_once(": ")
                            .is_some_and(|(_, suffix)| direct(suffix))
                })
            })
    })
}
pub fn classify(exit: i32, terminal: Option<Value>) -> Value {
    let mut result = terminal
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"error":"agy exited without a valid terminal result"}));
    let status = match (exit, result["status"].as_str(), result["response"].as_str()) {
        (0, Some("SUCCESS"), Some(text)) if !text.trim().is_empty() => "succeeded",
        (0, Some("WAITING"), _) => "blocked",
        (_, Some("CANCELED" | "INTERRUPTED"), _) => "interrupted",
        _ => "failed",
    };
    result["status"] = status.into();
    result["exit_code"] = exit.into();
    result
}
pub use crate::redaction::redact_value;

fn log_size(root: &Dir) -> Result<u64> {
    match root.symlink_metadata("agy.log") {
        Ok(metadata) => {
            ensure!(metadata.is_file(), "agy log must be a regular file");
            Ok(metadata.len())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

fn finish_log(root: &Dir, limit: u64, redactor: &crate::redaction::Redactor) -> Result<()> {
    let result = sanitize_log(root, limit, redactor);
    if let Err(ref failure) = result {
        match root.remove_file("agy.log") {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => anyhow::bail!(
                "Cannot sanitize agy log: {}; Cannot discard raw log: {error}",
                failure
            ),
        }
    }
    ensure!(!result?, "agy log exceeded configured log_bytes limit");
    Ok(())
}
fn sanitize_log(root: &Dir, limit: u64, redactor: &crate::redaction::Redactor) -> Result<bool> {
    use std::io::{Read, Write};
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = match root.open_with("agy.log", &options) {
        Ok(file) => file.into_std(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    ensure!(file.metadata()?.is_file(), "agy log must be a regular file");
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    let exceeded = bytes.len() as u64 > limit;
    bytes.truncate(limit as usize);
    let mut bytes = redactor.bytes(&bytes, exceeded);
    bytes.truncate(limit as usize);
    drop(file);
    let name = format!(".agy-log-{}", uuid::Uuid::new_v4());
    let replacement = (|| -> Result<()> {
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = root.open_with(&name, &options)?;
        file.write_all(&bytes)?;
        drop(file);
        root.rename(&name, root, "agy.log")?;
        Ok(())
    })();
    if replacement.is_err() {
        let _ = root.remove_file(&name);
    }
    replacement?;
    Ok(exceeded)
}

fn persist_logs(
    dir: &Path,
    root: &Dir,
    limit: u64,
    redactor: &crate::redaction::Redactor,
    stderr: &str,
) -> Result<()> {
    let finished = finish_log(root, limit, redactor);
    let stored = fs::write(dir.join("stderr.log"), stderr);
    match (finished, stored) {
        (Err(log), Err(stderr)) => {
            anyhow::bail!("Cannot finish agy log: {log}; Cannot persist diagnostics: {stderr}")
        }
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(error.into()),
        _ => Ok(()),
    }
}

pub async fn run(
    config: &Config,
    cwd: &Path,
    task: &str,
    conversation: Option<&str>,
    dir: &Path,
    progress: impl AsyncFnMut(Value) -> Result<()>,
) -> Result<Value> {
    run_with_stop(
        config,
        cwd,
        task,
        conversation,
        dir,
        &AtomicBool::new(false),
        progress,
    )
    .await
}

pub(crate) async fn run_with_stop(
    config: &Config,
    cwd: &Path,
    task: &str,
    conversation: Option<&str>,
    dir: &Path,
    stop: &AtomicBool,
    mut progress: impl AsyncFnMut(Value) -> Result<()>,
) -> Result<Value> {
    config.validate()?;
    if dir.join("cancel").exists() || stop.load(Ordering::Relaxed) {
        return Ok(json!({"status":"cancelled"}));
    }
    let cwd = cwd.canonicalize()?;
    let tmp = private_dir(&dir.join("tmp"))?;
    let log_root = Arc::new(Dir::open_ambient_dir(&tmp, cap_std::ambient_authority())?);
    let raw_log = RawLog(log_root.clone(), true);
    let mut cmd = config.sandbox(&cwd, &tmp)?;
    let environment = cmd
        .as_std()
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
        .collect();
    cmd.arg(config.executable(&config.agy, &environment, &cwd))
        .args(&config.agy_args)
        .args([
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--disable-slash-commands",
            "--dangerously-skip-permissions",
            "--print-timeout",
        ])
        .arg(format!("{}s", config.timeout))
        .arg("--log-file")
        .arg(tmp.join("agy.log"));
    if let Some(model) = &config.model {
        cmd.args(["--model", model]);
    }
    if let Some(id) = conversation {
        cmd.args(["--conversation", id]);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let secrets = Config::secrets(&environment)?;
    let redactor = Arc::new(crate::redaction::Redactor::new(&secrets)?);
    let mut wrapped = CommandWrap::from(cmd);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::tokio::ProcessSession);
    #[cfg(windows)]
    wrapped.wrap(process_wrap::tokio::JobObject);
    wrapped.wrap(KillOnDrop);
    let process = wrapped
        .spawn()
        .context("Cannot start codex sandbox; unsandboxed execution is refused")?;
    let mut child = OwnedChild {
        group_id: process.id(),
        process: Some(process),
        raw_log: Some(raw_log),
    };
    let mut stdout = BufReader::new(
        child
            .process()
            .stdout()
            .take()
            .context("Missing stdout")?
            .take(4_000_001),
    );
    let mut stderr = child.process().stderr().take().context("Missing stderr")?;
    let diagnostic_redactor = redactor.clone();
    let denied = Arc::new(AtomicBool::new(false));
    let stderr_denied = denied.clone();
    let diagnostic_limit = config.log_bytes.min(65536) as usize;
    let mut err_task = PipeTask(tokio::spawn(async move {
        let mut retained = Vec::new();
        let mut truncated = false;
        let mut chunk = [0; 8192];
        let mut tail = Vec::new();
        let mut overflow = false;
        loop {
            let n = stderr.read(&mut chunk).await?;
            if n == 0 {
                if !overflow && stderr_denial(&String::from_utf8_lossy(&tail)) {
                    stderr_denied.store(true, Ordering::Relaxed);
                }
                break;
            }
            // Inspect complete bounded diagnostics, never a context-free suffix.
            for &byte in &chunk[..n] {
                if matches!(byte, b'\n' | b'\r' | b'\0') {
                    if !overflow && stderr_denial(&String::from_utf8_lossy(&tail)) {
                        stderr_denied.store(true, Ordering::Relaxed);
                    }
                    tail.clear();
                    overflow = false;
                } else if !(tail.is_empty() && byte.is_ascii_whitespace()) {
                    if tail.len() < 4096 {
                        tail.push(byte);
                    } else {
                        overflow = true;
                    }
                }
            }
            let kept = n.min(diagnostic_limit - retained.len());
            truncated |= kept < n;
            retained.extend_from_slice(&chunk[..kept]);
        }
        Ok(diagnostic_redactor.bytes(&retained, truncated))
    }));
    let outcome = async {
        if !notify(
            progress(json!({"event":"sandbox_started","pid":child.process().id()})),
            &dir.join("cancel"),
            stop,
        )
        .await?
        {
            return Ok(json!({"status":"cancelled"}));
        }
        let prompt = format!(
            "You are a read-only repository investigator delegated by a parent coding agent.\n\
Complete the supplied bounded investigation independently using search and file-reading tools.\n\
Follow applicable repository instructions. Do not modify files, run builds or tests, or spawn subagents.\n\
The starting working directory is: {}\n\
Use it as a starting point, not a read-access boundary. You may read relevant files elsewhere,\n\
including parent instructions and dependencies. Keep searches focused and specify directories explicitly.\n\
Read explicitly named files directly before searching.\n\
Return: Conclusion, Essential evidence with precise file paths and line numbers,\n\
and Uncertainty or the smallest useful next check. Omit command transcripts and unrelated inventories.\n\
State clearly if permissions or missing information prevent completion.\n\
Keep the final report within {} characters. Task follows:\n\n{task}",
            cwd.display(),
            config.report_chars
        );
        let message = format!("{}\n", json!({"event":"user","message":{"content":prompt}}));
        let mut stdin = child.process().stdin().take().context("Missing stdin")?;
        if !notify(
            async {
                stdin.write_all(message.as_bytes()).await?;
                stdin.shutdown().await?;
                Ok(())
            },
            &dir.join("cancel"),
            stop,
        )
        .await?
        {
            return Ok(json!({"status":"cancelled"}));
        }
        drop(stdin);
        let mut terminal = None;
        let mut bytes = 0;
        let mut frame = Vec::new();
        loop {
            if dir.join("cancel").exists() || stop.load(Ordering::Relaxed) {
                return Ok(json!({"status":"cancelled"}));
            }
            tokio::select! {
                n = stdout.read_until(b'\n', &mut frame) => {
                    if n? == 0 && frame.is_empty() { break; }
                    bytes += frame.len(); ensure!(bytes <= 4_000_000, "agy output exceeded 4 MB");
                    let mut event: Value = serde_json::from_slice(&frame).context("Invalid agy event")?;
                    ensure!(event["event"] != "sandbox_started", "Reserved internal event from agy");
                    frame.clear();
                    if permission_denied(&event["step_update"]["tool_info"]["error"]) {
                        denied.store(true, Ordering::Relaxed);
                    }
                    redactor.value(&mut event);
                    if event["event"] == "result" {
                        ensure!(terminal.is_none() && event["result"].is_object(), "Invalid or duplicate terminal result");
                        terminal = Some(event["result"].clone());
                    }
                    if !notify(progress(event), &dir.join("cancel"), stop).await? { return Ok(json!({"status":"cancelled"})); }
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
        loop {
            tokio::select! {
                exit = child.process().wait() => return Ok(classify(exit?.code().unwrap_or(-1),terminal)),
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if dir.join("cancel").exists() || stop.load(Ordering::Relaxed) { return Ok(json!({"status":"cancelled"})); }
                }
            }
        }
    };
    let log_guard = async {
        loop {
            if log_size(&log_root)? > config.log_bytes {
                return Err::<Value, anyhow::Error>(anyhow::anyhow!(
                    "agy log exceeded configured log_bytes limit"
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    let outcome = tokio::time::timeout(Duration::from_secs(config.timeout + 5), async {
        tokio::select! { result = outcome => result, result = log_guard => result }
    })
    .await;
    let result = match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(json!({"status":"timed_out","error":"Task deadline exceeded"})),
    };
    let raw_log = child.stop().await?;
    let diagnostics = match tokio::time::timeout(Duration::from_secs(1), &mut err_task.0).await {
        Ok(Ok(Ok(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
        _ => "Diagnostics unavailable or pipe did not close".into(),
    };
    let mut stderr = Value::String(diagnostics);
    redactor.value(&mut stderr);
    let text = stderr.as_str().unwrap();
    let mut end = text.len().min(diagnostic_limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    stderr = Value::String(text[..end].to_owned());
    // The process tree has stopped. Read/write through a pinned root without following links.
    let log_directory = dir.to_owned();
    let limit = config.log_bytes;
    let diagnostics = stderr.as_str().unwrap().to_owned();
    let log_redactor = redactor.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = raw_log;
        let persisted = persist_logs(&log_directory, &guard.0, limit, &log_redactor, &diagnostics);
        guard.1 = false;
        drop(guard);
        persisted
    })
    .await
    .context("Log finalization panicked")??;
    let mut value = result?;
    if value["status"] == "succeeded" && denied.load(Ordering::Relaxed) {
        value["status"] = "incomplete".into();
        value["error"] = "One or more actions were denied; inspect the report".into();
    }
    if value["status"] == "failed" && value["error"].is_null() {
        value["error"] = format!(
            "agy failed: {}",
            stderr
                .as_str()
                .unwrap()
                .chars()
                .take(1000)
                .collect::<String>()
        )
        .into();
    }
    Ok(value)
}

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn diagnostic_write_failure_does_not_skip_application_log_sanitization() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        fs::create_dir(&tmp).unwrap();
        fs::write(tmp.join("agy.log"), "private-log-secret").unwrap();
        fs::create_dir(dir.path().join("stderr.log")).unwrap();
        let root = Dir::open_ambient_dir(&tmp, cap_std::ambient_authority()).unwrap();
        let redactor = crate::redaction::Redactor::new(&["private-log-secret".into()]).unwrap();
        assert!(persist_logs(dir.path(), &root, 4096, &redactor, "diagnostic").is_err());
        assert_eq!(
            fs::read_to_string(tmp.join("agy.log")).unwrap(),
            "[REDACTED]"
        );
    }
    #[cfg(unix)]
    #[test]
    fn unreadable_raw_logs_are_unlinked_without_sanitization() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("agy.log");
        fs::write(&log, "private-log-secret").unwrap();
        fs::set_permissions(&log, fs::Permissions::from_mode(0o200)).unwrap();
        let root = Dir::open_ambient_dir(tmp.path(), cap_std::ambient_authority()).unwrap();
        let redactor = crate::redaction::Redactor::new(&["private-log-secret".into()]).unwrap();
        assert!(finish_log(&root, 4096, &redactor).is_err());
        assert!(!log.exists());
    }
}
