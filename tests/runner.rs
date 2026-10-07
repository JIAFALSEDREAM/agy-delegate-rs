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
