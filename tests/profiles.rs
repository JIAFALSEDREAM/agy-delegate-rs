#![cfg(unix)]
mod common;
use common::CommandExt;
use serde_json::Value;
use std::{fs, path::Path, process::Command};

fn command(state: &Path, home: &Path, script: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"));
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("CODEX_HOME", home.join("codex"))
        .arg("--state-dir")
        .arg(state)
        .arg("--codex")
        .arg(common::backend(state))
        .args(["--agy", "/bin/sh", "--agy-arg"])
        .arg(script)
        .args(["--timeout", "5"]);
    command
}
fn report(output: std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("CLI must return a stored result")
}
const PARSE: &str = "profile=\nresume=\nwhile [ $# -gt 0 ]; do\ncase \"$1\" in\n--gemini_dir) shift; profile=$1;;\n--conversation) shift; resume=$1;;\nesac\nshift\ndone\n[ -n \"$profile\" ] || exit 3\nread input\n";
const SUCCESS: &str = "printf '%s\\n' '{\"event\":\"result\",\"conversation_id\":\"fe708f0a-a74b-4a92-ad7b-d55ccae82c08\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"fixture\"}}'\n";

#[test]
fn private_profiles_reuse_history_and_preserve_shared_login() {
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let shared = home.path().join(".gemini/antigravity-cli");
    fs::create_dir_all(&shared).unwrap();
    fs::write(shared.join("settings.json"), "{}").unwrap();
    fs::write(shared.join("antigravity-oauth-token"), "fixture-login").unwrap();
    fs::write(shared.join("personal.db"), "keep").unwrap();
    let script = state.path().join("agy.sh");
    fs::write(&script, format!("{PARSE}\n[ -z \"$resume\" ] || [ -f \"$profile/history\" ] || exit 4\nprintf history > \"$profile/history\"\n{SUCCESS}")).unwrap();
    let first = report(
        command(state.path(), home.path(), &script)
            .arg("run")
            .arg("--cwd")
            .arg(repo.path())
            .args(["--task", "first"])
            .bounded_output(),
    );
    assert_eq!(first["status"], "succeeded");
    let id = first["id"].as_str().unwrap();
    let profile = state.path().join("conversations").join(id);
    assert_eq!(
        fs::read(profile.join("antigravity-cli/antigravity-oauth-token")).unwrap(),
        b"fixture-login"
    );
    assert!(!profile.join("antigravity-cli/personal.db").exists());
    let followup = report(
        command(state.path(), home.path(), &script)
            .args(["followup", id, "second"])
            .bounded_output(),
    );
    let second = report(
        command(state.path(), home.path(), &script)
            .args(["wait", followup["id"].as_str().unwrap(), "--seconds", "5"])
            .bounded_output(),
    );
    assert_eq!(second["status"], "succeeded");
    assert_eq!(second["root_id"], id);
    assert_eq!(
        fs::read(shared.join("antigravity-oauth-token")).unwrap(),
        b"fixture-login"
    );
    assert_eq!(fs::read(shared.join("settings.json")).unwrap(), b"{}");
    assert_eq!(fs::read(shared.join("personal.db")).unwrap(), b"keep");
}

#[test]
fn aggregate_quota_stops_a_running_process_and_rejects_fast_success() {
    for (ending, expected) in [
        ("exec sleep 30\n", "failed"),
        (SUCCESS, "failed"),
        (
            "printf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"fixture\"}}'\necho backend-fixture-error >&2\nexit 7\n",
            "failed",
        ),
        (
            "touch \"$profile/../../jobs/${profile##*/}/cancel\"\nexec sleep 30\n",
            "cancelled",
        ),
    ] {
        let state = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let script = state.path().join("agy.sh");
        fs::write(&script, format!("{PARSE}echo $$ > \"$profile/pid\"\nmkdir \"$profile/nested\"\ntruncate -s 600000 \"$profile/one\"\ntruncate -s 600000 \"$profile/nested/two\"\n{ending}")).unwrap();
        let result = report(
            command(state.path(), home.path(), &script)
                .args(["--profile-bytes", "1000000", "run", "--cwd"])
                .arg(repo.path())
                .args(["--task", "quota"])
                .bounded_output(),
        );
        assert_eq!(result["status"], expected);
        assert!(result["error"].as_str().unwrap().contains("profile_bytes"));
        if ending.contains("backend-fixture-error") {
            assert!(
                result["error"]
                    .as_str()
                    .unwrap()
                    .contains("backend-fixture-error")
            );
        }
        let pid = state
            .path()
            .join("conversations")
            .join(result["id"].as_str().unwrap())
            .join("pid");
        {
            let pid =
                fs::read_to_string(pid).expect("fixture must publish PID before exceeding quota");
            let pid: i32 = pid.trim().parse().unwrap();
            assert_eq!(unsafe { libc::kill(-pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
    }
}
