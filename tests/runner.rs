#![cfg(unix)]
use agy_delegate_rs::{config::Config, runner};
use std::{os::unix::fs::PermissionsExt, time::Duration};

async fn fake(body: &str) -> anyhow::Result<serde_json::Value> {
    let state = tempfile::tempdir()?;
    let repo = tempfile::tempdir()?;
    let backend = state.path().join("backend");
    std::fs::write(&backend, format!("#!/bin/sh\ncat >/dev/null\n{body}\n"))?;
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700))?;
    let cfg = Config {
        codex: backend,
        agy: "agy".into(),
        agy_env_file: None,
        model: None,
        timeout: 1,
        state_dir: None,
        ..Config::default()
    };
    tokio::time::timeout(
        Duration::from_secs(9),
        runner::run(
            &cfg,
            repo.path(),
            "inspect",
            None,
            state.path(),
            async |_| Ok(()),
        ),
    )
    .await?
}
const SUCCESS: &str = "printf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\"}}'";

#[tokio::test]
async fn ownership_acknowledgement_precedes_task_delivery_and_is_cancellable() {
    for ordinary_event in [false, true] {
        let state = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let backend = state.path().join("backend");
        let input = state.path().join("input");
        std::fs::write(
            &backend,
            format!(
                "#!/bin/sh\ncat > '{}'\nprintf '%s\\n' '{{\"event\":\"progress\"}}'\nsleep 30\n",
                input.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cfg = Config {
            codex: backend,
            agy: "agy".into(),
            agy_env_file: None,
            model: None,
            timeout: 30,
            state_dir: None,
            ..Config::default()
        };
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(
                runner::run(
                    &cfg,
                    repo.path(),
                    "inspect",
                    None,
                    state.path(),
                    async |event| {
                        if ordinary_event && event["event"] == "sandbox_started" {
                            return Ok(());
                        }
                        std::future::pending::<anyhow::Result<()>>().await
                    }
                ),
                async {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    if !ordinary_event {
                        assert!(std::fs::read(&input).unwrap_or_default().is_empty());
                    }
                    std::fs::write(state.path().join("cancel"), "cancel").unwrap();
                }
            )
            .0
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["status"], "cancelled");
    }
}

#[tokio::test]
async fn inherited_stderr_pipe_cannot_hang_a_completed_task() {
    let result = fake(&format!("sleep 30 >/dev/null &\n{SUCCESS}"))
        .await
        .unwrap();
    assert_eq!(result["status"], "succeeded");
}

#[tokio::test]
async fn unterminated_result_survives_cancelled_reads() {
    let result = fake(&format!("{}\nsleep 1", SUCCESS.replace("%s\\n", "%s")))
        .await
        .unwrap();
    assert_eq!(result["status"], "succeeded");
}
#[tokio::test]
async fn stderr_is_drained_and_non_utf8_is_lossy() {
    let result = fake(&format!(
        "printf '\\377' >&2\nhead -c 100000 /dev/zero >&2\n{SUCCESS}"
    ))
    .await
    .unwrap();
    assert_eq!(result["status"], "succeeded");
}
#[tokio::test]
async fn oversized_unterminated_frame_is_rejected() {
    let error = fake("head -c 5000000 /dev/zero").await.unwrap_err();
    assert!(error.to_string().contains("exceeded 4 MB"));
}
#[tokio::test]
async fn non_object_terminal_is_protocol_failure() {
    assert!(
        fake("printf '%s\\n' '{\"event\":\"result\",\"result\":[]}'")
            .await
            .unwrap_err()
            .to_string()
            .contains("Invalid or duplicate terminal")
    );
}

#[tokio::test]
async fn backend_cannot_spoof_process_ownership() {
    assert!(
        fake("printf '%s\\n' '{\"event\":\"sandbox_started\",\"pid\":null}'")
            .await
            .unwrap_err()
            .to_string()
            .contains("Reserved internal event")
    );
}

#[test]
fn relative_codex_uses_host_directory() {
    let host = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    for (dir, response) in [(host.path(), "trusted"), (repo.path(), "untrusted")] {
        let backend = dir.join("backend");
        std::fs::write(&backend,format!("#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"status\":\"SUCCESS\",\"response\":\"{response}\"}}}}'\n")).unwrap();
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .current_dir(host.path())
        .arg("--codex")
        .arg("./backend")
        .arg("--state-dir")
        .arg(state.path())
        .arg("run")
        .arg("--cwd")
        .arg(repo.path())
        .args(["--task", "inspect"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["response"], "trusted");
}

#[test]
fn relative_project_and_agy_resolve_to_the_same_directory() {
    let host = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let project = host.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let codex = host.path().join("codex");
    std::fs::write(&codex,"#!/bin/sh\nwhile [ \"$1\" != \"--\" ]; do\nif [ \"$1\" = \"-C\" ]; then shift; cd \"$1\" || exit; fi\nshift\ndone\nshift\nexec \"$@\"\n").unwrap();
    let agy = project.join("backend");
    std::fs::write(&agy, format!("#!/bin/sh\ncat >/dev/null\n{SUCCESS}\n")).unwrap();
    for executable in [&codex, &agy] {
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .current_dir(host.path())
        .args(["--codex", "./codex", "--agy", "./backend"])
        .arg("--state-dir")
        .arg(state.path())
        .args(["run", "--cwd", "project", "--task", "inspect"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn cancellation_after_stdout_eof_stops_the_task() {
    for blocked_stdin in [false, true] {
        let state = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let backend = state.path().join("backend");
        let body = if blocked_stdin {
            "#!/bin/sh\nsleep 30\n"
        } else {
            "#!/bin/sh\ncat >/dev/null\nexec 1>&-\nsleep 30\n"
        };
        std::fs::write(&backend, body).unwrap();
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cfg = Config {
            codex: backend,
            agy: "agy".into(),
            agy_env_file: None,
            model: None,
            timeout: 30,
            state_dir: None,
            ..Config::default()
        };
        let cancel = state.path().join("cancel");
        let task = if blocked_stdin {
            "x".repeat(100_000)
        } else {
            "inspect".into()
        };
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(
                runner::run(&cfg, repo.path(), &task, None, state.path(), async |_| Ok(
                    ()
                )),
                async {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    std::fs::write(cancel, "cancel").unwrap();
                }
            )
            .0
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["status"], "cancelled");
    }
}
