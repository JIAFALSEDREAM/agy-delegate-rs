use serde_json::{Value, json};
use std::process::Stdio;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{Duration, timeout},
};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}
impl Client {
    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        timeout(Duration::from_secs(10), async {
            self.stdin
                .write_all(
                    format!(
                        "{}\n",
                        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            loop {
                let line = self.stdout.next_line().await.unwrap().unwrap();
                let value: Value = serde_json::from_str(&line).unwrap();
                if value["id"] == id {
                    return value;
                }
            }
        })
        .await
        .expect("MCP request deadline exceeded")
    }
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_status_and_wait_preserve_complete_unicode_reports_with_legacy_config() {
    use std::os::unix::fs::PermissionsExt;

    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join("AGENTS.md"),
        "Project rules mention characters.",
    )
    .unwrap();
    let config = state.path().join("config.toml");
    std::fs::write(&config, "report_chars = 4000\n").unwrap();
    let report = format!(
        "{}\n关键尾部证据：src/end.rs:42 ✅\n不确定性：仍需真实环境核验。🦀",
        "Unicode 证据 e\u{301}😀\n".repeat(700)
    );
    assert!(report.chars().count() > 4000);
    let event = state.path().join("result.json");
    std::fs::write(
        &event,
        format!("{}\n", json!({"event":"result","result":{"status":"SUCCESS","response":report,"conversation_id":"fixture"}})),
    )
    .unwrap();
    let backend = state.path().join("backend");
    std::fs::write(
        &backend,
        "#!/bin/sh\ncat > \"$PROMPT_CAPTURE\"\ncat \"$REPORT_EVENT\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let capture = state.path().join("prompt.json");
    let mut child = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--config")
        .arg(&config)
        .arg("--state-dir")
        .arg(state.path())
        .arg("--codex")
        .arg(&backend)
        .arg("mcp")
        .env("REPORT_EVENT", &event)
        .env("PROMPT_CAPTURE", &capture)
        .env("CODEX_HOME", state.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut client = Client {
        child,
        stdin,
        stdout,
    };
    let initialized = client.request(1,"initialize",json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}})).await;
    assert!(initialized.get("error").is_none());
    client
        .stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let submitted = client
        .request(
            2,
            "tools/call",
            json!({"name":"agy_submit","arguments":{"task":"inspect","cwd":repo.path()}}),
        )
        .await;
    let id = submitted["result"]["structuredContent"]["id"]
        .as_str()
        .unwrap();
    for (request_id, name, arguments) in [
        (3, "agy_wait", json!({"job_id":id,"seconds":5})),
        (4, "agy_status", json!({"job_id":id})),
    ] {
        let response = client
            .request(
                request_id,
                "tools/call",
                json!({"name":name,"arguments":arguments}),
            )
            .await;
        assert_ne!(response["result"]["isError"], true, "{response}");
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["status"], "succeeded");
        assert_eq!(structured["report"], report);
        assert_eq!(structured["report_truncated"], false);
        let text: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(&text, structured);
    }
    let prompt: Value = serde_json::from_slice(&std::fs::read(capture).unwrap()).unwrap();
    let prompt = prompt["message"]["content"].as_str().unwrap();
    assert!(prompt.contains("Project rules mention characters."));
    let guidance = prompt.rsplit_once("</loaded_instructions>\n").unwrap().1;
    let guidance = guidance.split_once("Task follows:\n\n").unwrap().0;
    assert!(guidance.contains("Keep the final report concise"));
    assert!(!guidance.contains("characters"));
    for action in ["status", "wait", "report"] {
        let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
            .arg("--config")
            .arg(&config)
            .arg("--state-dir")
            .arg(state.path())
            .args(["--report-chars", "0", action, id])
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        if action == "report" {
            assert_eq!(String::from_utf8(output.stdout).unwrap(), report);
        } else {
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["report"], report);
            assert_eq!(value["report_truncated"], false);
        }
    }
    drop(client.stdin);
    assert!(
        timeout(Duration::from_secs(5), client.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn stdio_tools_preserve_failed_job_semantics() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--state-dir")
        .arg(state.path())
        .arg("--codex")
        .arg(state.path().join("missing-codex"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut client = Client {
        child,
        stdin,
        stdout,
    };
    let initialized = client.request(1,"initialize",json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}})).await;
    assert!(initialized.get("error").is_none());
    assert!(
        initialized["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("incomplete")
    );
    client
        .stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let list = client.request(2, "tools/list", json!({})).await;
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 6);
    for tool in list["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert_eq!(
            tool["annotations"]["readOnlyHint"],
            matches!(name, "agy_wait" | "agy_status" | "agy_list")
        );
        assert_eq!(
            tool["annotations"]["openWorldHint"],
            matches!(name, "agy_submit" | "agy_followup")
        );
    }
    let submit = client
        .request(
            3,
            "tools/call",
            json!({"name":"agy_submit","arguments":{"task":"inspect","cwd":repo.path()}}),
        )
        .await;
    let job = &submit["result"]["structuredContent"];
    let waited = client
        .request(
            4,
            "tools/call",
            json!({"name":"agy_wait","arguments":{"job_id":job["id"],"seconds":5}}),
        )
        .await;
    assert_eq!(waited["result"]["structuredContent"]["status"], "failed");
    assert_ne!(waited["result"]["isError"], true);
    let invalid = client
        .request(
            5,
            "tools/call",
            json!({"name":"agy_wait","arguments":{"job_id":job["id"],"seconds":56}}),
        )
        .await;
    assert!(invalid.get("error").is_some());
    let broken_dir = state.path().join("broken-cancel");
    std::fs::create_dir_all(broken_dir.join("cancel")).unwrap();
    let broken_id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let data = json!({"id":broken_id,"root_id":broken_id,"status":"running","local_log_dir":broken_dir,"created":0,"updated":now});
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO jobs VALUES(?1,?1,'running',?2,?3)",
        rusqlite::params![broken_id, now, data.to_string()],
    )
    .unwrap();
    let operational = client
        .request(
            7,
            "tools/call",
            json!({"name":"agy_cancel","arguments":{"job_id":broken_id}}),
        )
        .await;
    assert!(operational.get("error").is_none());
    assert_eq!(operational["result"]["isError"], true);
    db.execute("DELETE FROM jobs WHERE id=?1", [broken_id])
        .unwrap();
    let recovered = client
        .request(
            6,
            "tools/call",
            json!({"name":"agy_list","arguments":{"limit":1}}),
        )
        .await;
    assert_eq!(
        recovered["result"]["structuredContent"]["result"][0]["id"],
        job["id"]
    );
    assert_eq!(
        recovered["result"]["structuredContent"]["result"][0]
            .as_object()
            .unwrap()
            .len(),
        5
    );
    let content: Value =
        serde_json::from_str(recovered["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(content.is_array());
    client.child.kill().await.unwrap();
    client.child.wait().await.unwrap();
}
