use crate::config::{Config, private_dir};
use anyhow::{Context, Result, ensure};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Stdio, sync::Arc, time::Duration};
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
struct OwnedChild(Box<dyn ChildWrapper>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}
struct PipeTask(tokio::task::JoinHandle<std::io::Result<Vec<u8>>>);
impl Drop for PipeTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn notify(future: impl Future<Output = Result<()>>, cancel: &Path) -> Result<bool> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result.map(|()|true),
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if cancel.exists() { return Ok(false); }
            }
        }
    }
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

pub async fn run(
    config: &Config,
    cwd: &Path,
    task: &str,
    conversation: Option<&str>,
    dir: &Path,
    mut progress: impl AsyncFnMut(Value) -> Result<()>,
) -> Result<Value> {
    if dir.join("cancel").exists() {
        return Ok(json!({"status":"cancelled"}));
    }
    let cwd = cwd.canonicalize()?;
    let tmp = private_dir(&dir.join("tmp"))?;
    let mut cmd = config.sandbox(&cwd, &tmp)?;
    let environment = cmd
        .as_std()
        .get_envs()
        .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
        .collect();
    cmd.arg(config.executable(&config.agy, &environment, &cwd))
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
    let secrets: Vec<_> = cmd
        .as_std()
        .get_envs()
        .filter_map(|(key, value)| {
            let key = key.to_string_lossy();
            value
                .filter(|v| {
                    v.len() >= 8
                        && (key.contains("KEY") || key.contains("TOKEN") || key.contains("SECRET"))
                })
                .map(|v| {
                    v.to_str()
                        .map(str::to_owned)
                        .context("Credential environment values must be UTF-8")
                })
        })
        .collect::<Result<Vec<_>>>()?;
    let redactor = Arc::new(crate::redaction::Redactor::new(&secrets)?);
    let mut wrapped = CommandWrap::from(cmd);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::tokio::ProcessSession);
    #[cfg(windows)]
    wrapped.wrap(process_wrap::tokio::JobObject);
    wrapped.wrap(KillOnDrop);
    let mut child = OwnedChild(
        wrapped
            .spawn()
            .context("Cannot start codex sandbox; unsandboxed execution is refused")?,
    );
    let mut stdout = BufReader::new(
        child
            .0
            .stdout()
            .take()
            .context("Missing stdout")?
            .take(4_000_001),
    );
    let mut stderr = child.0.stderr().take().context("Missing stderr")?;
    let diagnostic_redactor = redactor.clone();
    let mut err_task = PipeTask(tokio::spawn(async move {
        let mut retained = Vec::new();
        let mut truncated = false;
        let mut chunk = [0; 8192];
        loop {
            let n = stderr.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            let kept = n.min(65536 - retained.len());
            truncated |= kept < n;
            retained.extend_from_slice(&chunk[..kept]);
        }
        Ok(diagnostic_redactor.bytes(&retained, truncated))
    }));
    let outcome = async {
        if !notify(
            progress(json!({"event":"sandbox_started","pid":child.0.id()})),
            &dir.join("cancel"),
        )
        .await?
        {
            return Ok(json!({"status":"cancelled"}));
        }
        let prompt = format!(
            "Perform a read-only investigation and return a concise report with file/line evidence. Do not modify project files. Task:\n{task}"
        );
        let message = format!("{}\n", json!({"event":"user","message":{"content":prompt}}));
        let mut stdin = child.0.stdin().take().context("Missing stdin")?;
        if !notify(
            async {
                stdin.write_all(message.as_bytes()).await?;
                stdin.shutdown().await?;
                Ok(())
            },
            &dir.join("cancel"),
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
            if dir.join("cancel").exists() {
                return Ok(json!({"status":"cancelled"}));
            }
            tokio::select! {
                n = stdout.read_until(b'\n', &mut frame) => {
                    if n? == 0 && frame.is_empty() { break; }
                    bytes += frame.len(); ensure!(bytes <= 4_000_000, "agy output exceeded 4 MB");
                    let mut event: Value = serde_json::from_slice(&frame).context("Invalid agy event")?;
                    ensure!(event["event"] != "sandbox_started", "Reserved internal event from agy");
                    frame.clear(); redactor.value(&mut event);
                    if event["event"] == "result" {
                        ensure!(terminal.is_none() && event["result"].is_object(), "Invalid or duplicate terminal result");
                        terminal = Some(event["result"].clone());
                    }
                    if !notify(progress(event), &dir.join("cancel")).await? { return Ok(json!({"status":"cancelled"})); }
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
        loop {
            tokio::select! {
                exit = child.0.wait() => return Ok(classify(exit?.code().unwrap_or(-1),terminal)),
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if dir.join("cancel").exists() { return Ok(json!({"status":"cancelled"})); }
                }
            }
        }
    };
    let outcome = tokio::time::timeout(Duration::from_secs(config.timeout + 5), outcome).await;
    let result = match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(json!({"status":"timed_out","error":"Task deadline exceeded"})),
    };
    let killed = child.0.start_kill();
    #[cfg(unix)]
    let killed = killed.or_else(|e| {
        if e.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(e)
        }
    });
    killed.context("Could not stop owned sandbox tree")?;
    tokio::time::timeout(Duration::from_secs(2), child.0.wait())
        .await
        .context("Sandbox cleanup timeout")??;
    let diagnostics = match tokio::time::timeout(Duration::from_secs(1), &mut err_task.0).await {
        Ok(Ok(Ok(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
        _ => "Diagnostics unavailable or pipe did not close".into(),
    };
    let mut stderr = Value::String(diagnostics);
    redactor.value(&mut stderr);
    fs::write(dir.join("stderr.log"), stderr.as_str().unwrap())?;
    let mut value = result?;
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
