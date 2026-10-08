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

#[tokio::test]
async fn install_is_idempotent_restores_policy_and_preserves_unrelated_config() {
    let fixture = Fixture::new();
    let agents = fixture.home.path().join("AGENTS.md");
    let original = "# Personal rules\nkeep this\n";
    fs::write(&agents, original).unwrap();
    let config = fixture.home.path().join("config.toml");
    fs::write(&config, "# personal comment\nmodel = 'personal-model'\n").unwrap();
    let mut integration = fixture.integration();
    assert_eq!(integration.install().await.unwrap()["installed"], true);
    assert!(
        fs::read_to_string(&agents)
            .unwrap()
            .contains(agy_delegate_rs::integration::POLICY)
    );
    integration.install().await.unwrap();
    assert_eq!(
        fs::read_to_string(fixture.home.path().join("operations")).unwrap(),
        "add\n"
    );
    integration.uninstall().await.unwrap();
    integration.uninstall().await.unwrap();
    assert_eq!(fs::read_to_string(&agents).unwrap(), original);
    let text = fs::read_to_string(config).unwrap();
    assert!(text.contains("# personal comment") && text.contains("personal-model"));
    assert!(!text.contains("mcp_servers.agy-delegate-rs"));
    assert!(fixture.state.path().exists());
}

#[tokio::test]
async fn outside_policy_edits_survive_and_managed_edits_block_uninstall() {
    let fixture = Fixture::new();
    let mut integration = fixture.integration();
    let agents = fixture.home.path().join("AGENTS.md");
    integration.install().await.unwrap();
    let edited = fs::read_to_string(&agents).unwrap() + "\n# Added rule\nkeep me\n";
    fs::write(&agents, &edited).unwrap();
    integration.uninstall().await.unwrap();
    assert!(fs::read_to_string(&agents).unwrap().contains("keep me"));
    integration.install().await.unwrap();
    fs::write(
        &agents,
        fs::read_to_string(&agents)
            .unwrap()
            .replace("quick_explorer", "custom-worker"),
    )
    .unwrap();
    assert!(
        integration
            .uninstall()
            .await
            .unwrap_err()
            .to_string()
            .contains("edited")
    );
    assert!(
        fs::read_to_string(fixture.home.path().join("config.toml"))
            .unwrap()
            .contains("mcp_servers.agy-delegate-rs")
    );
}

#[tokio::test]
async fn unrelated_registrations_legacy_policy_and_lost_backups_are_preserved() {
    let fixture = Fixture::new();
    let mut integration = fixture.integration();
    let config = fixture.home.path().join("config.toml");
    let original = "[mcp_servers.agy-delegate-rs]\ncommand='unrelated'\nargs=[]\n";
    fs::write(&config, original).unwrap();
    assert!(integration.register().await.is_err());
    assert!(integration.uninstall().await.is_err());
    assert_eq!(fs::read_to_string(&config).unwrap(), original);
    fs::write(&config, "").unwrap();
    let agents = fixture.home.path().join("AGENTS.md");
    fs::write(
        &agents,
        "<!-- agy-delegate:start -->old policy<!-- agy-delegate:end -->",
    )
    .unwrap();
    assert!(integration.install().await.is_err());
    assert!(!fs::read_to_string(&config).unwrap().contains("mcp_servers"));
    fs::write(&agents, "").unwrap();
    integration.install().await.unwrap();
    fs::remove_file(fixture.home.path().join(".agy-delegate-rs-policy.json")).unwrap();
    assert!(integration.install().await.is_err());
    assert!(integration.uninstall().await.is_err());
    assert!(
        fs::read_to_string(&agents)
            .unwrap()
            .contains(agy_delegate_rs::integration::POLICY)
    );
}

#[tokio::test]
async fn installation_without_existing_rules_removes_only_its_created_file() {
    let fixture = Fixture::new();
    let mut integration = fixture.integration();
    integration.install().await.unwrap();
    integration.uninstall().await.unwrap();
    assert!(!fixture.home.path().join("AGENTS.md").exists());
    assert!(
        !fixture
            .home
            .path()
            .join(".agy-delegate-rs-policy.json")
            .exists()
    );
}

#[tokio::test]
async fn interrupted_policy_write_is_recoverable_without_discarding_outside_edits() {
    let fixture = Fixture::new();
    let agents = fixture.home.path().join("AGENTS.md");
    fs::write(&agents, "keep original").unwrap();
    let mut integration = fixture.integration();
    integration.install().await.unwrap();
    fs::write(&agents, "keep original").unwrap();
    integration.install().await.unwrap();
    assert!(
        fs::read_to_string(&agents)
            .unwrap()
            .contains(agy_delegate_rs::integration::POLICY)
    );
    fs::write(&agents, "edited outside without marker").unwrap();
    assert!(integration.uninstall().await.is_err());
    assert_eq!(
        fs::read_to_string(agents).unwrap(),
        "edited outside without marker"
    );
}

#[tokio::test]
async fn redirected_policy_and_malformed_backup_are_preserved() {
    let fixture = Fixture::new();
    let victim = fixture.state.path().join("personal");
    fs::write(&victim, "keep").unwrap();
    let agents = fixture.home.path().join("AGENTS.md");
    std::os::unix::fs::symlink(&victim, &agents).unwrap();
    let mut integration = fixture.integration();
    assert!(integration.install().await.is_err());
    assert!(integration.uninstall().await.is_err());
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
    fs::remove_file(&agents).unwrap();
    integration.install().await.unwrap();
    fs::write(
        fixture.home.path().join(".agy-delegate-rs-policy.json"),
        "{}",
    )
    .unwrap();
    assert!(integration.uninstall().await.is_err());
    assert!(
        fs::read_to_string(&agents)
            .unwrap()
            .contains(agy_delegate_rs::integration::POLICY)
    );
}

#[test]
fn failed_install_doctor_does_not_register_or_write_rules() {
    let fixture = Fixture::new();
    let source = fs::read_to_string(&fixture.config.codex).unwrap().replace(
        "path = home / 'config.toml'",
        "with pathlib.Path(__file__).with_name('doctor-homes').open('a') as stream: stream.write(str(home) + '\\n')\npath = home / 'config.toml'",
    );
    fs::write(&fixture.config.codex, source).unwrap();
    let config = fixture.state.path().join("test-config.toml");
    fs::write(&config, toml::to_string(&fixture.config).unwrap()).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .env("HOME", fixture.home.path())
        .env("CODEX_HOME", fixture.state.path())
        .env("XDG_CONFIG_HOME", fixture.home.path().join("config"))
        .env("XDG_DATA_HOME", fixture.home.path().join("data"))
        .args(["--config"])
        .arg(config)
        .args(["install", "--codex-home"])
        .arg(fixture.home.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    let homes = fs::read_to_string(fixture.state.path().join("doctor-homes")).unwrap();
    assert_eq!(homes.lines().count(), 2);
    assert!(
        homes
            .lines()
            .all(|home| std::path::Path::new(home) == fixture.home.path())
    );
    for name in [
        "config.toml",
        "AGENTS.md",
        ".agy-delegate-rs-policy.json",
        "operations",
    ] {
        assert!(!fixture.home.path().join(name).exists());
    }
}

#[tokio::test]
async fn outside_rules_changed_during_codex_removal_survive() {
    let fixture = Fixture::new();
    let mut integration = fixture.integration();
    integration.install().await.unwrap();
    let backend = &fixture.config.codex;
    let source = fs::read_to_string(backend).unwrap().replace(
        "    text = path.read_text()\n    start =",
        "    agents = home / 'AGENTS.md'\n    agents.write_text(agents.read_text() + '\\n# rule added during removal\\n')\n    text = path.read_text()\n    start =",
    );
    fs::write(backend, source).unwrap();
    integration.uninstall().await.unwrap();
    let rules = fs::read_to_string(fixture.home.path().join("AGENTS.md")).unwrap();
    assert!(rules.contains("# rule added during removal"));
    assert!(!rules.contains(agy_delegate_rs::integration::POLICY));
}

#[tokio::test]
async fn interrupted_restoration_preserves_outside_rules_and_finishes_on_retry() {
    let fixture = Fixture::new();
    let mut integration = fixture.integration();
    integration.install().await.unwrap();
    let agents = fixture.home.path().join("AGENTS.md");
    let backup_path = fixture.home.path().join(".agy-delegate-rs-policy.json");
    let installed = fs::read_to_string(&agents).unwrap() + "\n# outside rule\n";
    let restored = installed.replace(agy_delegate_rs::integration::POLICY, "");
    let mut backup: serde_json::Value =
        serde_json::from_slice(&fs::read(&backup_path).unwrap()).unwrap();
    backup["restored"] = restored.clone().into();
    backup.as_object_mut().unwrap().remove("added");
    fs::write(&backup_path, backup.to_string()).unwrap();
    fs::write(&agents, &installed).unwrap();
    fs::write(fixture.home.path().join("config.toml"), "").unwrap();
    assert!(integration.install().await.is_err());
    assert_eq!(
        fs::read_to_string(fixture.home.path().join("operations")).unwrap(),
        "add\n"
    );
    fs::write(&agents, backup["original"].as_str().unwrap()).unwrap();
    assert!(integration.uninstall().await.is_err());
    assert!(integration.install().await.is_err());
    assert!(backup_path.exists());
    fs::write(&agents, &restored).unwrap();
    assert!(
        integration
            .install()
            .await
            .unwrap_err()
            .to_string()
            .contains("retry uninstall")
    );
    integration.uninstall().await.unwrap();
    assert_eq!(fs::read_to_string(&agents).unwrap(), restored);
    assert!(!backup_path.exists());
    integration.uninstall().await.unwrap();
    assert_eq!(fs::read_to_string(agents).unwrap(), restored);
}

#[tokio::test]
async fn large_rules_restore_without_duplicating_the_backup() {
    let fixture = Fixture::new();
    let original = "a".repeat(480_000);
    let agents = fixture.home.path().join("AGENTS.md");
    fs::write(&agents, &original).unwrap();
    let mut integration = fixture.integration();
    integration.install().await.unwrap();
    integration.uninstall().await.unwrap();
    assert_eq!(fs::read_to_string(agents).unwrap(), original);
}
