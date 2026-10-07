use crate::config::{Config, private_dir};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
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

async fn stop(child: &mut Child) -> Result<()> {
    if let Some(pid) = child.id() {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            let status = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await?;
            ensure!(status.success(), "Could not stop sandbox process tree");
        }
    }
    let _ = child.kill().await;
    child.wait().await?;
    Ok(())
}

pub fn classify(exit: i32, terminal: Option<Value>) -> Value {
    let mut result =
        terminal.unwrap_or_else(|| json!({"error":"agy exited without a terminal result"}));
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

pub async fn run(
    config: &Config,
    cwd: &Path,
    task: &str,
    conversation: Option<&str>,
    dir: &Path,
    mut progress: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    if dir.join("cancel").exists() {
        return Ok(json!({"status":"cancelled"}));
    }
    let tmp = private_dir(&dir.join("tmp"))?;
    let mut cmd = config.sandbox(cwd, &tmp)?;
    cmd.arg(&config.agy)
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
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    detach(&mut cmd);
    let mut child = cmd
        .spawn()
        .context("Cannot start codex sandbox; unsandboxed execution is refused")?;
    let mut stdout = BufReader::new(child.stdout.take().context("Missing stdout")?).lines();
    let stderr = child.stderr.take().context("Missing stderr")?;
    let err_task = tokio::spawn(async move {
        let mut text = String::new();
        stderr.take(64 * 1024).read_to_string(&mut text).await?;
        anyhow::Ok(text)
    });
    let secrets: Vec<_> = config
        .environment()?
        .into_iter()
        .filter(|(k, v)| {
            v.len() >= 8 && (k.contains("KEY") || k.contains("TOKEN") || k.contains("SECRET"))
        })
        .map(|(_, v)| v)
        .collect();
    let redact = |mut text: String| {
        for secret in &secrets {
            text = text.replace(secret, "[REDACTED]");
        }
        text
    };
    let outcome = async {
        let prompt = format!(
            "Perform a read-only investigation and return a concise report with file/line evidence. Do not modify project files. Task:\n{task}"
        );
        let message = format!("{}\n", json!({"event":"user","message":{"content":prompt}}));
        let mut stdin = child.stdin.take().context("Missing stdin")?;
        stdin.write_all(message.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);
        let mut terminal = None;
        let mut bytes = 0;
        loop {
            if dir.join("cancel").exists() {
                return Ok(json!({"status":"cancelled"}));
            }
            tokio::select! {
                line = stdout.next_line() => {
                    let Some(line) = line? else { break };
                    bytes += line.len(); ensure!(bytes <= 4_000_000, "agy output exceeded 4 MB");
                    let line = redact(line);
                    let event: Value = serde_json::from_str(&line).context("Invalid agy event")?;
                    if event["event"] == "result" {
                        ensure!(terminal.is_none(), "Duplicate terminal result");
                        terminal = Some(event["result"].clone());
                    }
                    progress(event)?;
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if dir.join("cancel").exists() { return Ok(json!({"status":"cancelled"})); }
                }
            }
        }
        let exit = child.wait().await?.code().unwrap_or(-1);
        Ok(classify(exit, terminal))
    };
    let outcome = tokio::time::timeout(Duration::from_secs(config.timeout + 5), outcome).await;
    let result = match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(json!({"status":"timed_out","error":"Task deadline exceeded"})),
    };
    if child.try_wait()?.is_none() {
        stop(&mut child).await?;
    }
    let stderr = redact(err_task.await??);
    fs::write(dir.join("stderr.log"), &stderr)?;
    let mut value = result?;
    if value["status"] == "failed" && value["error"].is_null() {
        value["error"] = format!(
            "agy failed: {}",
            stderr.chars().take(1000).collect::<String>()
        )
        .into();
    }
    Ok(value)
}
