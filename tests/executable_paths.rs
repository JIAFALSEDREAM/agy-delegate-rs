#![cfg(unix)]
use agy_delegate_rs::config::Config;
use std::{fs, process::Command};

#[test]
fn env_file_only_path_resolves_programs_without_exporting_credentials() {
    const CHILD: &str = "AGY_EXECUTABLE_PATH_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .env_remove("PATH")
            .env(CHILD, "1")
            .args([
                "--exact",
                "env_file_only_path_resolves_programs_without_exporting_credentials",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    for name in ["codex", "agy"] {
        std::os::unix::fs::symlink("/bin/sh", root.path().join(name)).unwrap();
    }
    let environment = root.path().join("api.env");
    fs::write(
        &environment,
        format!(
            "PATH={}\nAGY_TEST_SECRET=private-fixture-value\n",
            root.path().display()
        ),
    )
    .unwrap();
    let mut config = Config {
        state_dir: Some(root.path().join("state")),
        agy_env_file: Some(environment.clone()),
        ..Config::default()
    };
    let lookup = config.lookup_environment().unwrap();
    assert!(!lookup.contains_key(std::ffi::OsStr::new("AGY_TEST_SECRET")));
    config.absolutize().unwrap();
    assert!(config.agy.is_absolute() && config.codex.is_absolute());
    assert!(
        !toml::to_string(&config)
            .unwrap()
            .contains("private-fixture-value")
    );
    fs::remove_file(environment).unwrap();
    config.absolutize().unwrap();
}

#[test]
fn timeout_after_the_subcommand_is_applied() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .env("HOME", root.path())
        .args(["init", "--timeout", "123", "--config"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved: Config = toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(saved.timeout, 123);
}
