use agy_delegate_rs::{config::Config, jobs::Jobs};
use clap::{CommandFactory, FromArgMatches, Parser};
use serde_json::json;
use std::{path::Path, process::Command};

#[derive(Parser)]
struct Arguments {
    #[command(flatten)]
    config: Config,
}

fn load(path: &Path, args: &[&str]) -> anyhow::Result<Config> {
    let matches = Arguments::command()
        .try_get_matches_from(std::iter::once("fixture").chain(args.iter().copied()))?;
    Config::load(
        Some(path),
        Arguments::from_arg_matches(&matches)?.config,
        &matches,
    )
}

#[test]
fn configuration_paths_and_explicit_cli_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(&file, "timeout = 12\nmodel = 'configured'\nstate_dir = 'state'\nagy_env_file = 'api.env'\ncodex = './bin/codex'\nreport_chars = 300\n").unwrap();
    let configured = load(&file, &[]).unwrap();
    assert_eq!(configured.timeout, 12);
    assert_eq!(configured.state_dir.unwrap(), dir.path().join("state"));
    assert_eq!(configured.agy_env_file.unwrap(), dir.path().join("api.env"));
    assert_eq!(configured.codex, dir.path().join("./bin/codex"));
    assert_eq!(configured.report_chars, 300);
    let overridden = load(
        &file,
        &[
            "--timeout",
            "600",
            "--model",
            "override",
            "--state-dir",
            "cli-state",
            "--report-chars",
            "4000",
        ],
    )
    .unwrap();
    assert_eq!(overridden.timeout, 600); // Explicit defaults must still override the file.
    assert_eq!(overridden.model.as_deref(), Some("override"));
    assert_eq!(overridden.state_dir.unwrap(), Path::new("cli-state"));
    assert_eq!(overridden.report_chars, 4000);
}

#[test]
fn initialization_never_overwrites_or_copies_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nested/config.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .env("FIXTURE_API_KEY", "secret-that-must-not-be-copied")
        .arg("init")
        .arg("--config")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let contents = std::fs::read(&file).unwrap();
    assert!(!String::from_utf8_lossy(&contents).contains("secret-that-must-not-be-copied"));
    assert_eq!(load(&file, &[]).unwrap().retention_days, 14);
    assert!(Config::default().initialize(&file).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), contents);
    assert_eq!(
        std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
        1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn malformed_unknown_and_out_of_range_settings_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    for input in [
        "log_bytes = 4095",
        "log_bytes = 32000001",
        "profile_bytes = 999999",
        "profile_bytes = 1000000001",
        "timeout = 0",
        "report_chars = 6001",
        "retention_days = 366",
        "max_history_jobs = 9",
        "state_dir = ''",
        "codex = ''",
        "secret = 'sensitive-fixture'",
        "model = 'sensitive-fixture",
    ] {
        std::fs::write(&file, input).unwrap();
        let error = load(&file, &[]).unwrap_err().to_string();
        assert!(!error.contains("sensitive-fixture"));
    }
    assert!(load(&dir.path().join("missing"), &[]).is_err());
}

#[test]
fn wrapper_arguments_and_log_limit_use_explicit_cli_precedence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "agy = 'node'\nagy_args = ['/path/to/agy.js', '--wrapper']\nlog_bytes = 8192\n",
    )
    .unwrap();
    let config = load(&path, &[]).unwrap();
    assert_eq!(config.agy_args, ["/path/to/agy.js", "--wrapper"]);
    let config = load(
        &path,
        &[
            "--agy-arg",
            "alternate.js",
            "--agy-arg",
            "--prefix",
            "--log-bytes",
            "4096",
        ],
    )
    .unwrap();
    assert_eq!(config.agy_args, ["alternate.js", "--prefix"]);
    assert_eq!(config.log_bytes, 4096);
}

#[test]
fn cli_loads_file_before_opening_the_task_database() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(&file, "state_dir = 'file-state'\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .args(["--config"])
        .arg(&file)
        .arg("list")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(dir.path().join("file-state/jobs.sqlite3").exists());
    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--config")
        .arg(&file)
        .arg("--state-dir")
        .arg(dir.path().join("override"))
        .arg("list")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(dir.path().join("override/jobs.sqlite3").exists());
}

#[test]
fn legacy_job_settings_remain_readable_and_reports_use_the_current_limit() {
    let legacy: Config = serde_json::from_value(json!({"codex":"codex","agy":"agy","agy_env_file":null,"model":null,"timeout":2,"state_dir":null})).unwrap();
    assert_eq!(legacy.max_history_jobs, 1000);
    assert_eq!(legacy.profile_bytes, 128_000_000);
    let state = tempfile::tempdir().unwrap();
    let jobs = Jobs::new(Config {
        state_dir: Some(state.path().into()),
        report_chars: 256,
        ..legacy
    })
    .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let report = "汉".repeat(300);
    let db = rusqlite::Connection::open(state.path().join("jobs.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO jobs VALUES(?1,?1,'succeeded',0,?2)",
        rusqlite::params![
            id,
            json!({"id":id,"status":"succeeded","report":report}).to_string()
        ],
    )
    .unwrap();
    let view = jobs.get(&id).unwrap();
    assert_eq!(view["report"].as_str().unwrap().chars().count(), 256);
    assert_eq!(view["report_truncated"], true);
}
