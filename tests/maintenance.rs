use serde_json::Value;
use std::process::Command;

fn invoke(
    state: &std::path::Path,
    config: &std::path::Path,
    args: &[&str],
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .arg("--config")
        .arg(config)
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn doctor_reports_configuration_and_environment_failures_without_secret_values() {
    let state = tempfile::tempdir().unwrap();
    let config = state.path().join("config.toml");
    std::fs::write(&config, "secret = 'private-fixture-value'").unwrap();
    let output = invoke(state.path(), &config, &["doctor"]);
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["checks"][0]["name"], "configuration");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-fixture-value"));
    std::fs::write(&config, "agy_env_file = 'missing.env'").unwrap();
    let output = invoke(state.path(), &config, &["doctor"]);
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["checks"][0]["name"], "environment");
}

#[cfg(unix)]
#[test]
fn doctor_rejects_an_unsandboxed_backend_and_only_uses_agy_help() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let config = state.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let agy = state.path().join("agy");
    let codex = state.path().join("codex");
    std::fs::write(&agy, "#!/bin/sh\ncase \"$1\" in\n--version) printf 'agy 1.2.17+private-version-fixture'; printf 'Warning: private-diagnostic-fixture' >&2;;\n--help) echo '--input-format --output-format --disable-slash-commands --dangerously-skip-permissions --print-timeout --log-file --conversation' >&2;;\n*) exit 99;;\nesac\n").unwrap();
    std::fs::write(&codex, "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'codex-cli 0.160.0-alpha.1' >&2; exit; fi\nwhile [ \"$1\" != -- ]; do shift; done\nshift\nexec \"$@\"\n").unwrap();
    for path in [&agy, &codex] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let output = invoke(
        state.path(),
        &config,
        &[
            "--agy",
            agy.to_str().unwrap(),
            "--codex",
            codex.to_str().unwrap(),
            "doctor",
        ],
    );
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    for name in ["agy", "codex", "agy_headless", "state_directory"] {
        assert_eq!(
            report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|check| check["name"] == name)
                .unwrap()["ok"],
            true
        );
    }
    assert_eq!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == "sandbox")
            .unwrap()["ok"],
        false
    );
    assert_eq!(report["model_call"], "not_tested");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-diagnostic-fixture"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-version-fixture"));
    assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 3);
}

#[test]
#[ignore = "requires installed Codex and a usable OS sandbox"]
fn native_doctor_checks_the_sandbox_without_a_model_call() {
    let state = tempfile::tempdir().unwrap();
    let config = state.path().join("config.toml");
    std::fs::write(&config, "").unwrap();
    let output = invoke(
        state.path(),
        &config,
        &["--agy", "missing-agy-fixture", "doctor"],
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == "sandbox")
            .unwrap()["ok"],
        true,
        "{report}"
    );
    assert_eq!(report["model_call"], "not_tested");
    assert!(!state.path().join("jobs.sqlite3").exists());
}
