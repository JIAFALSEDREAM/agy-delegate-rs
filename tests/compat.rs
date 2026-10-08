use agy_delegate_rs::{config::Config, jobs::Jobs};
use serde_json::json;
use std::{
    path::Path,
    process::{Command, Output},
};

fn cli(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn complete_report_export_preserves_unicode_and_existing_files() {
    let state = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(Config {
        state_dir: Some(state.path().into()),
        ..Config::default()
    })
    .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let report = "证据\n".repeat(3000);
    let directory = jobs.state.join("jobs").join(&id);
    std::fs::create_dir(&directory).unwrap();
    let data = json!({"id":id,"root_id":id,"status":"incomplete","report":report,"progress":"x".repeat(200),"local_log_dir":directory});
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO jobs VALUES(?1,?1,'incomplete',0,?2)",
        rusqlite::params![id, data.to_string()],
    )
    .unwrap();
    assert_eq!(jobs.get(&id).unwrap()["report_truncated"], true);
    let printed = cli(state.path(), &["report", &id]);
    assert!(printed.status.success());
    assert_eq!(String::from_utf8(printed.stdout).unwrap(), report);
    let output = state.path().join("report.txt");
    let args = ["report", &id, "--output", output.to_str().unwrap()];
    assert!(cli(state.path(), &args).status.success());
    std::fs::write(&output, "keep").unwrap();
    assert!(!cli(state.path(), &args).status.success());
    assert_eq!(std::fs::read_to_string(output).unwrap(), "keep");
    let compact = jobs.list(1).unwrap();
    assert_eq!(
        compact[0]["progress"].as_str().unwrap().chars().count(),
        120
    );
    assert!(
        jobs.cleanup(false).unwrap()["deleted_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(id))
    );
}

#[test]
fn task_files_reject_invalid_empty_oversized_or_conflicting_input_before_start() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let input = state.path().join("task.txt");
    for (bytes, diagnostic) in [
        (vec![], "Task must contain"),
        (vec![255], "invalid utf-8"),
        (vec![b'x'; 96001], "Task file exceeds 96000 bytes"),
        (vec![b'x'; 24001], "Task must contain"),
    ] {
        std::fs::write(&input, bytes).unwrap();
        let result = cli(
            state.path(),
            &[
                "submit",
                "--cwd",
                repo.path().to_str().unwrap(),
                "--task-file",
                input.to_str().unwrap(),
            ],
        );
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains(diagnostic));
        assert!(!state.path().join("jobs").exists());
        assert!(!state.path().join("jobs.sqlite3").exists());
    }
    std::fs::write(&input, "inspect").unwrap();
    assert!(
        !cli(
            state.path(),
            &[
                "submit",
                "--cwd",
                repo.path().to_str().unwrap(),
                "--task",
                "inspect",
                "--task-file",
                input.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
}

#[cfg(unix)]
#[test]
fn multiline_task_file_reaches_backend_and_followup_in_the_same_conversation() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let backend = state.path().join("backend");
    std::fs::write(&backend, "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$TASK_CAPTURE.args\"\ncat > \"$TASK_CAPTURE\"\nprintf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\",\"conversation_id\":\"fixture\"}}'\n").unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let input = state.path().join("task.txt");
    let capture = state.path().join("input.json");
    std::fs::write(&input, "检查\n第二行").unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
            .arg("--state-dir")
            .arg(state.path())
            .arg("--codex")
            .arg(&backend)
            .env("TASK_CAPTURE", &capture)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let job = run(&[
        "submit",
        "--cwd",
        repo.path().to_str().unwrap(),
        "--task-file",
        input.to_str().unwrap(),
    ]);
    assert_eq!(
        run(&["wait", job["id"].as_str().unwrap(), "--seconds", "5"])["status"],
        "succeeded"
    );
    let delivered: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&capture).unwrap()).unwrap();
    assert!(
        delivered["message"]["content"]
            .as_str()
            .unwrap()
            .ends_with("检查\n第二行")
    );
    std::fs::write(&input, "进一步检查").unwrap();
    let follow = run(&[
        "followup",
        job["id"].as_str().unwrap(),
        "--task-file",
        input.to_str().unwrap(),
    ]);
    assert_eq!(follow["root_id"], job["root_id"]);
    assert_eq!(
        run(&["wait", follow["id"].as_str().unwrap(), "--seconds", "5"])["status"],
        "succeeded"
    );
    assert!(
        std::fs::read_to_string(capture.with_extension("json.args"))
            .unwrap()
            .contains("--conversation\nfixture\n")
    );
    let delivered: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&capture).unwrap()).unwrap();
    assert!(
        delivered["message"]["content"]
            .as_str()
            .unwrap()
            .ends_with("进一步检查")
    );
    let boundary = "😀".repeat(24000);
    std::fs::write(&input, &boundary).unwrap();
    assert_eq!(
        run(&[
            "run",
            "--cwd",
            repo.path().to_str().unwrap(),
            "--task-file",
            input.to_str().unwrap()
        ])["status"],
        "succeeded"
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state.path())
        .arg("--codex")
        .arg(&backend)
        .env("TASK_CAPTURE", &capture)
        .args([
            "run",
            "--cwd",
            repo.path().to_str().unwrap(),
            "--task-file",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(boundary.as_bytes())
        .unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    let delivered: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&capture).unwrap()).unwrap();
    assert!(
        delivered["message"]["content"]
            .as_str()
            .unwrap()
            .ends_with(&boundary)
    );
}
