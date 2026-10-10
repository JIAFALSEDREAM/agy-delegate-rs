#![cfg(unix)]
// Each integration-test binary uses a different subset of this shared fixture.
#![allow(dead_code)]
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

pub struct Captured {
    child: Child,
    stdout: tempfile::NamedTempFile,
    stderr: tempfile::NamedTempFile,
}
impl Captured {
    pub fn id(&self) -> u32 {
        self.child.id()
    }
}
impl Drop for Captured {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub trait CommandExt {
    fn capture(&mut self) -> Captured;
    fn bounded_output(&mut self) -> Output {
        wait(self.capture())
    }
}
impl CommandExt for Command {
    fn capture(&mut self) -> Captured {
        let stdout = tempfile::NamedTempFile::new().unwrap();
        let stderr = tempfile::NamedTempFile::new().unwrap();
        let child = self
            .stdout(Stdio::from(stdout.as_file().try_clone().unwrap()))
            .stderr(Stdio::from(stderr.as_file().try_clone().unwrap()))
            .spawn()
            .unwrap();
        Captured {
            child,
            stdout,
            stderr,
        }
    }
}
fn snapshot(file: &mut tempfile::NamedTempFile) -> Vec<u8> {
    let length = file.as_file().metadata().unwrap().len().min(1_048_576);
    file.as_file_mut().seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    file.as_file_mut()
        .take(length)
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}
pub fn wait(mut captured: Captured) -> Output {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = captured.child.try_wait().unwrap() {
            // Regular capture files avoid pipe backpressure and descendant-held EOF waits.
            return Output {
                status,
                stdout: snapshot(&mut captured.stdout),
                stderr: snapshot(&mut captured.stderr),
            };
        }
        assert!(
            Instant::now() < deadline,
            "CLI process exceeded test deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
pub fn backend(state: &Path) -> std::path::PathBuf {
    let path = state.join("codex");
    let mut fixture = tempfile::NamedTempFile::new_in(state).unwrap();
    fixture
        .write_all(
            r#"#!/bin/sh
if [ "$1" = --version ]; then
    echo 'codex-cli 0.160.0'
    exit 0
fi
while [ "$#" -gt 0 ] && [ "$1" != -- ]; do
    if [ "$1" = -C ]; then
        shift
        cd "$1" || exit 99
    fi
    shift
done
[ "$#" -gt 0 ] || exit 99
shift
exec "$@"
"#
            .as_bytes(),
        )
        .unwrap();
    fixture
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o700))
        .unwrap();
    // A wait/followup command must never truncate a script a worker may execute.
    // Publish a complete fixture once, including for concurrent constructors.
    match fixture.into_temp_path().persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("Cannot publish backend fixture: {error}"),
    }
    path
}
pub fn invoke(state: &Path, repo: &Path, script: &Path, task: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state)
        .arg("--codex")
        .arg(backend(state))
        .args(["--agy", "/bin/sh", "--agy-arg"])
        .arg(script)
        .args(["--log-bytes", "4096", "run", "--cwd"])
        .arg(repo)
        .args(["--task", task])
        .bounded_output()
}
