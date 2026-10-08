#![cfg(unix)]
mod common;
use common::CommandExt;
use std::process::Command;

#[test]
fn capture_handles_output_larger_than_a_pipe_buffer() {
    let output = Command::new("/bin/sh")
        .args(["-c", "head -c 131072 /dev/zero"])
        .bounded_output();
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 131072);
}
#[test]
fn descendants_holding_output_do_not_delay_a_completed_command() {
    let start = std::time::Instant::now();
    let output = Command::new("/bin/sh")
        .args(["-c", "sleep 1 & printf done"])
        .bounded_output();
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
    assert_eq!(output.stdout, b"done");
}
