use agy_delegate_rs::redaction::{Redactor, redact_value};
use serde_json::json;

#[test]
fn overlapping_credentials_are_redacted_against_original_text() {
    for (text, secrets) in [
        (
            "abcdefgh-private-suffix",
            vec!["abcdefgh", "abcdefgh-private-suffix"],
        ),
        ("abcdefghijk", vec!["abcdefgh", "defghijk"]),
        ("ababababa", vec!["abababa"]),
        ("你好机密后缀", vec!["你好机密", "机密后缀"]),
    ] {
        let mut value = json!({"value":text});
        redact_value(
            &mut value,
            &secrets.into_iter().map(String::from).collect::<Vec<_>>(),
        );
        assert_eq!(value["value"], "[REDACTED]");
    }
}

#[test]
fn truncated_overlap_uses_original_match_ranges() {
    let redactor = Redactor::new(&["abcdefgh".into(), "fghijklm".into()]).unwrap();
    assert_eq!(
        redactor.bytes(b"padding-abcdefghij", true),
        b"padding-[REDACTED]"
    );
}

#[test]
fn repetitive_near_matches_do_not_block_redaction() {
    let secret = format!("{}b", "a".repeat(10000));
    let input = vec![b'a'; 3_900_000];
    let redactor = Redactor::new(&[secret]).unwrap();
    assert_eq!(redactor.bytes(&input, false), input);
}

#[test]
fn dense_matches_are_merged_without_collecting_every_occurrence() {
    let secrets = (8..40).map(|n| "a".repeat(n)).collect::<Vec<_>>();
    let redactor = Redactor::new(&secrets).unwrap();
    assert_eq!(redactor.bytes(&vec![b'a'; 3_900_000], false), b"[REDACTED]");
}

#[test]
fn truncated_suffix_matching_handles_many_credentials() {
    let secrets = (0..512)
        .map(|i| format!("credential-{i:04}"))
        .collect::<Vec<_>>();
    let redactor = Redactor::new(&secrets).unwrap();
    let mut input = vec![b'x'; 65530];
    input.extend_from_slice(b"cred");
    let mut expected = vec![b'x'; 65530];
    expected.extend_from_slice(b"[REDACTED]");
    assert_eq!(redactor.bytes(&input, true), expected);
}

#[test]
fn colliding_redacted_keys_preserve_every_value() {
    let mut value = json!({"abcdefgh":"first finding","ijklmnop":"second finding","[REDACTED]":"existing marker"});
    redact_value(&mut value, &["abcdefgh".into(), "ijklmnop".into()]);
    let object = value.as_object().unwrap();
    assert_eq!(object.len(), 3);
    for text in ["first finding", "second finding", "existing marker"] {
        assert!(object.values().any(|value| value == text));
    }
    assert!(!object.contains_key("abcdefgh"));
    assert!(!object.contains_key("ijklmnop"));
}

#[cfg(unix)]
#[tokio::test]
async fn stderr_boundary_does_not_retain_a_credential_prefix() {
    use agy_delegate_rs::{config::Config, runner};
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let secret = "PRIVATE_FIXTURE_SECRET_SUFFIX";
    let env = state.path().join("api.env");
    std::fs::write(&env, format!("FIXTURE_API_KEY={secret}\n")).unwrap();
    let backend = state.path().join("backend");
    std::fs::write(&backend,format!("#!/bin/sh\ncat >/dev/null\nhead -c 65530 /dev/zero >&2\nprintf '%s' '{secret}' >&2\nprintf '%s\\n' '{{\"event\":\"result\",\"result\":{{\"status\":\"SUCCESS\",\"response\":\"ok\"}}}}'\n")).unwrap();
    std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cfg = Config {
        codex: backend,
        agy: "agy".into(),
        agy_env_file: Some(env),
        model: None,
        timeout: 2,
        state_dir: None,
    };
    let result = runner::run(
        &cfg,
        repo.path(),
        "inspect",
        None,
        state.path(),
        async |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "succeeded");
    let diagnostic = std::fs::read_to_string(state.path().join("stderr.log")).unwrap();
    assert!(!diagnostic.contains("PRIVAT"));
}

#[cfg(unix)]
#[test]
fn non_utf8_credentials_are_rejected_before_launch() {
    use std::os::unix::ffi::OsStringExt;
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agy-delegate-rs"))
        .env(
            "FIXTURE_API_KEY",
            std::ffi::OsString::from_vec(b"abcdefgh\xffijklmnop".to_vec()),
        )
        .arg("--state-dir")
        .arg(state.path())
        .arg("--codex")
        .arg("missing-backend")
        .arg("run")
        .arg("--cwd")
        .arg(repo.path())
        .args(["--task", "inspect"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("Credential environment values must be UTF-8"));
    assert!(!error.contains("abcdefgh"));
}
