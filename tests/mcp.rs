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
    client
        .stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let list = client.request(2, "tools/list", json!({})).await;
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 6);
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
    client.child.kill().await.unwrap();
    client.child.wait().await.unwrap();
}
