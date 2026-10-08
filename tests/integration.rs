#![cfg(unix)]
use agy_delegate_rs::{config::Config, integration::Integration};
use std::{fs, os::unix::fs::PermissionsExt};

struct Fixture {
    state: tempfile::TempDir,
    home: tempfile::TempDir,
    config: Config,
}
impl Fixture {
    fn new() -> Self {
        let state = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let backend = state.path().join("codex");
        fs::write(&backend, r##"#!/usr/bin/env python3
import json, os, pathlib, sys
home = pathlib.Path(os.environ['CODEX_HOME'])
path = home / 'config.toml'
args = sys.argv[1:]
if args[:3] == ['mcp','add','agy-delegate-rs']:
    command = args[4]
    argv = args[5:]
    text = path.read_text() if path.exists() else ''
    path.write_text(text + '\n[mcp_servers.agy-delegate-rs]\ncommand = ' + json.dumps(command) + '\nargs = ' + json.dumps(argv) + '\n')
elif args[:3] == ['mcp','remove','agy-delegate-rs']:
    text = path.read_text()
    start = text.index('[mcp_servers.agy-delegate-rs]')
    end = text.find('\n[', start + 1)
    path.write_text(text[:start] + (text[end:] if end >= 0 else ''))
else:
    sys.exit(99)
with (home / 'operations').open('a') as stream: stream.write(args[1] + '\n')
"##).unwrap();
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o700)).unwrap();
        let config = Config {
            codex: backend,
            agy: "/bin/sh".into(),
            state_dir: Some(state.path().into()),
            timeout: 123,
            ..Config::default()
        };
        Self {
            state,
            home,
            config,
        }
    }
    fn integration(&self) -> Integration {
        Integration::new(self.config.clone(), Some(self.home.path())).unwrap()
    }
}

#[tokio::test]
async fn register_is_idempotent_and_uninstall_preserves_unrelated_settings() {
    let fixture = Fixture::new();
    let config = fixture.home.path().join("config.toml");
    fs::write(&config, "# personal comment\nmodel = 'personal-model'\n").unwrap();
    let mut integration = fixture.integration();
    let registration = integration.register().await.unwrap();
    integration.register().await.unwrap();
    assert_eq!(
        fs::read_to_string(fixture.home.path().join("operations")).unwrap(),
        "add\n"
    );
    let saved: Config = toml::from_str(
        &fs::read_to_string(std::path::Path::new(
            registration["config"].as_str().unwrap(),
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(saved.timeout, 123);
    assert!(saved.state_dir.unwrap().is_absolute());
    integration.uninstall().await.unwrap();
    integration.uninstall().await.unwrap();
    let text = fs::read_to_string(&config).unwrap();
    assert!(text.contains("# personal comment") && text.contains("personal-model"));
    assert!(!text.contains("mcp_servers.agy-delegate-rs"));
}

#[tokio::test]
async fn unrelated_and_python_registrations_are_preserved() {
    let fixture = Fixture::new();
    let mut integration = fixture.integration();
    let path = fixture.home.path().join("config.toml");
    let original = "[mcp_servers.agy-delegate-rs]\ncommand='unrelated'\nargs=[]\n";
    fs::write(&path, original).unwrap();
    assert!(integration.register().await.is_err());
    assert!(integration.uninstall().await.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    let legacy = "[mcp_servers.agy-delegate]\ncommand='python'\nargs=[]\n";
    fs::write(&path, legacy).unwrap();
    assert!(integration.register().await.is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), legacy);
}
#[test]
fn concurrent_integration_and_redirected_locks_are_rejected() {
    let fixture = Fixture::new();
    let lease = fixture.integration();
    assert!(Integration::new(fixture.config.clone(), Some(fixture.home.path())).is_err());
    drop(lease);
    fs::remove_file(fixture.home.path().join(".agy-delegate-rs.lock")).unwrap();
    std::os::unix::fs::symlink(
        fixture.state.path().join("unrelated"),
        fixture.home.path().join(".agy-delegate-rs.lock"),
    )
    .unwrap();
    assert!(Integration::new(fixture.config.clone(), Some(fixture.home.path())).is_err());
    assert!(!fixture.state.path().join("unrelated").exists());
}

#[tokio::test]
async fn homes_have_independent_snapshots_and_registration_ignores_missing_credentials() {
    let fixture = Fixture::new();
    let second = tempfile::tempdir().unwrap();
    let mut first = fixture.integration();
    let a = first.register().await.unwrap();
    let original = fs::read(a["config"].as_str().unwrap()).unwrap();
    let mut config = fixture.config.clone();
    config.timeout = 456;
    config.agy_env_file = Some(fixture.state.path().join("missing.env"));
    let mut second = Integration::new(config, Some(second.path())).unwrap();
    let b = second.register().await.unwrap();
    assert_ne!(a["config"], b["config"]);
    assert_eq!(fs::read(a["config"].as_str().unwrap()).unwrap(), original);
    second.uninstall().await.unwrap();
    assert!(fixture.home.path().join("config.toml").is_file());
}

#[tokio::test]
async fn cli_only_uninstall_stops_the_selected_state_without_changing_codex_config() {
    let fixture = Fixture::new();
    let id = uuid::Uuid::new_v4().to_string();
    let directory = fixture.state.path().join("jobs").join(&id);
    let jobs = agy_delegate_rs::jobs::Jobs::new(fixture.config.clone()).unwrap();
    fs::create_dir(&directory).unwrap();
    let record =
        serde_json::json!({"id":id,"root_id":id,"status":"starting","local_log_dir":directory});
    let db = rusqlite::Connection::open(fixture.state.path().join("jobs.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO jobs VALUES(?1,?1,'starting',0,?2)",
        rusqlite::params![id, record.to_string()],
    )
    .unwrap();
    let path = fixture.home.path().join("config.toml");
    fs::write(&path, "# keep unrelated configuration\n").unwrap();
    fixture.integration().uninstall().await.unwrap();
    assert_eq!(jobs.get(&id).unwrap()["status"], "cancelled");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "# keep unrelated configuration\n"
    );
    assert!(!fixture.home.path().join("operations").exists());
}

#[tokio::test]
async fn replacement_registration_during_shutdown_is_preserved() {
    let fixture = Fixture::new();
    let mut integration = fixture.integration();
    integration.register().await.unwrap();
    agy_delegate_rs::jobs::Jobs::new(fixture.config.clone()).unwrap();
    let cleaner = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(fixture.state.path().join("jobs/.cleanup.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&cleaner).unwrap();
    let mut removing = Box::pin(integration.uninstall());
    tokio::select! {
        result = &mut removing => panic!("Uninstall bypassed cleaner: {result:?}"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
    }
    let path = fixture.home.path().join("config.toml");
    let replacement = "[mcp_servers.agy-delegate-rs]\ncommand='replacement'\nargs=[]\n";
    fs::write(&path, replacement).unwrap();
    drop(cleaner);
    assert!(removing.await.is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), replacement);
    assert_eq!(
        fs::read_to_string(fixture.home.path().join("operations")).unwrap(),
        "add\n"
    );
}

#[test]
fn registration_resolves_codex_from_env_file_path_without_exporting_credentials() {
    let fixture = Fixture::new();
    let environment = fixture.state.path().join("api.env");
    fs::write(
        &environment,
        format!(
            "PATH={}:/usr/bin:/bin\nAGY_REGISTRATION_SECRET=private-fixture-value\n",
            fixture.state.path().display()
        ),
    )
    .unwrap();
    let mut config = fixture.config.clone();
    config.codex = "codex".into();
    config.agy_env_file = Some(environment);
    let path = fixture.state.path().join("config.toml");
    fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .env_remove("PATH")
        .env("HOME", fixture.home.path())
        .args(["--config"])
        .arg(path)
        .args(["register", "--codex-home"])
        .arg(fixture.home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let saved = fs::read_to_string(report["config"].as_str().unwrap()).unwrap();
    assert!(!saved.contains("private-fixture-value"));
    let saved: Config = toml::from_str(&saved).unwrap();
    assert!(saved.codex.is_absolute());
}
