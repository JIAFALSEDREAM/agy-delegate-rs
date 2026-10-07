use crate::config::{Config, private_dir};
use anyhow::{Context, Result, ensure};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

struct ProbeChild(Box<dyn ChildWrapper>);
impl Drop for ProbeChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

async fn read(stream: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    stream.take(131073).read_to_end(&mut output).await?;
    ensure!(output.len() <= 131072, "Probe output exceeded limit");
    Ok(output)
}

async fn capture(command: Command) -> Result<(Vec<u8>, Vec<u8>)> {
    capture_with_timeout(command, Duration::from_secs(10)).await
}

async fn capture_with_timeout(
    mut command: Command,
    deadline: Duration,
) -> Result<(Vec<u8>, Vec<u8>)> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut wrapped = CommandWrap::from(command);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::tokio::ProcessSession);
    #[cfg(windows)]
    wrapped.wrap(process_wrap::tokio::JobObject);
    wrapped.wrap(KillOnDrop);
    let mut child = ProbeChild(wrapped.spawn()?);
    let result = tokio::time::timeout(deadline, async {
        let stdout = child.0.stdout().take().context("Missing probe stdout")?;
        let stderr = child.0.stderr().take().context("Missing probe stderr")?;
        let (stdout, stderr, status) = tokio::try_join!(read(stdout), read(stderr), async {
            Ok::<_, anyhow::Error>(child.0.wait().await?)
        })?;
        ensure!(status.success(), "Probe command failed");
        Ok((stdout, stderr))
    })
    .await
    .context("Probe timed out");
    let killed = child.0.start_kill();
    #[cfg(unix)]
    let killed = killed.or_else(|error| {
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    });
    killed.context("Cannot stop owned probe tree")?;
    tokio::time::timeout(Duration::from_secs(2), child.0.wait())
        .await
        .context("Probe cleanup timed out")??;
    result?
}

fn version(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output)
        .split_whitespace()
        .find_map(|word| {
            let candidate = word.trim_start_matches('v').split(['-', '+']).next()?;
            let parts: Vec<_> = candidate.split('.').collect();
            (parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
            .then(|| candidate.to_owned())
        })
}

pub fn probe(file: &Path, nonce: &str) -> Result<Value> {
    ensure!(
        fs::read_to_string(file)? == nonce,
        "Sandbox cannot read project fixture"
    );
    ensure!(
        fs::OpenOptions::new().write(true).open(file).is_err(),
        "Sandbox permits project writes"
    );
    let attempt = file
        .parent()
        .context("Missing fixture parent")?
        .join("write-probe");
    ensure!(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(attempt)
            .is_err(),
        "Sandbox permits project file creation"
    );
    let mut temporary =
        tempfile::NamedTempFile::new().context("Sandbox cannot create temporary files")?;
    temporary.write_all(b"probe")?;
    Ok(json!({"project_write_denied":true,"temp_write_allowed":true,"marker":nonce}))
}

async fn sandbox(config: &Config, state: &Path) -> Result<()> {
    let fixture = tempfile::Builder::new()
        .prefix("doctor-")
        .tempdir_in(state)?;
    let project = private_dir(&fixture.path().join("project"))?;
    let tmp = private_dir(&fixture.path().join("tmp"))?;
    let file = project.join("marker");
    let nonce = uuid::Uuid::new_v4().to_string();
    fs::write(&file, &nonce)?;
    let mut command = config.sandbox(&project, &tmp)?;
    command
        .arg(std::env::current_exe()?)
        .arg("sandbox-probe")
        .arg("--file")
        .arg(&file)
        .arg("--nonce")
        .arg(&nonce);
    let response: Value = serde_json::from_slice(&capture(command).await?.0)?;
    ensure!(
        response["project_write_denied"] == true
            && response["temp_write_allowed"] == true
            && response["marker"] == nonce,
        "Invalid sandbox probe response"
    );
    ensure!(
        fs::read_to_string(file)? == nonce,
        "Sandbox changed project fixture"
    );
    Ok(())
}

pub async fn diagnose(config: &Config) -> Value {
    let mut checks = vec![json!({"name":"configuration","ok":true})];
    let environment = match config.environment() {
        Ok(environment) => environment,
        Err(_) => {
            return json!({"ok":false,"checks":[{"name":"environment","ok":false,"detail":"Cannot load agy environment file; contents are not logged"}]});
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return json!({"ok":false,"checks":[{"name":"working_directory","ok":false}]}),
    };
    for (name, program) in [("agy", &config.agy), ("codex", &config.codex)] {
        let mut command = Command::new(config.executable(program, &environment, &cwd));
        command.envs(&environment).arg("--version");
        let found = match capture(command).await {
            Ok((stdout, stderr)) => version(&stdout).or_else(|| version(&stderr)),
            Err(_) => None,
        };
        checks.push(json!({"name":name,"ok":found.is_some(),"version_core":found,"detail":"Numeric version core; executable must be available"}));
    }
    let mut help = Command::new(config.executable(&config.agy, &environment, &cwd));
    help.envs(&environment).arg("--help");
    let flags = [
        "--input-format",
        "--output-format",
        "--disable-slash-commands",
        "--dangerously-skip-permissions",
        "--print-timeout",
        "--log-file",
        "--conversation",
    ];
    let supported = capture(help).await.is_ok_and(|(stdout, stderr)| {
        let stdout = String::from_utf8_lossy(&stdout);
        let stderr = String::from_utf8_lossy(&stderr);
        flags
            .iter()
            .all(|flag| stdout.contains(flag) || stderr.contains(flag))
    });
    checks.push(json!({"name":"agy_headless","ok":supported,"detail":"Required stream-json and conversation flags"}));
    let state = config.state();
    checks.push(json!({"name":"state_directory","ok":state.is_ok(),"detail":"Local task storage"}));
    let sandbox_ok = match state {
        Ok(state) => sandbox(config, &state).await.is_ok(),
        Err(_) => false,
    };
    checks.push(json!({"name":"sandbox","ok":sandbox_ok,"detail":"Read project, reject project writes, allow native temporary files"}));
    json!({"ok":checks.iter().all(|check|check["ok"] == true),"checks":checks,"model_call":"not_tested"})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prerelease_and_build_versions_only_expose_the_numeric_core() {
        for text in ["agy v1.2.17-alpha.1", "agy 1.2.17+private-version-fixture"] {
            assert_eq!(version(text.as_bytes()).as_deref(), Some("1.2.17"));
        }
        assert!(version(b"Warning: no usable version").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_probes_are_stopped_and_reaped_before_returning() {
        for overflow in [false, true] {
            let state = tempfile::tempdir().unwrap();
            let pidfile = state.path().join("pid");
            let script = if overflow {
                "echo $$ > \"$1\"; head -c 131073 /dev/zero; exec sleep 30"
            } else {
                "echo $$ > \"$1\"; exec sleep 30"
            };
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script, "probe"]).arg(&pidfile);
            let result = tokio::time::timeout(
                Duration::from_secs(4),
                capture_with_timeout(command, Duration::from_millis(300)),
            )
            .await
            .unwrap();
            let error = result.unwrap_err().to_string();
            assert!(error.contains(if overflow {
                "exceeded limit"
            } else {
                "Probe timed out"
            }));
            let pid: i32 = fs::read_to_string(pidfile).unwrap().trim().parse().unwrap();
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }
}
