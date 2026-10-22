//! The seccomp sandbox really blocks syscalls outside the allow-list.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_mcp-server-rs");
const SIGSYS: i32 = 31;

fn worker(job: &str, input: &str) -> std::process::Output {
    let mut child = Command::new(BIN)
        .args(["__sandbox-worker", job])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

#[test]
fn opening_a_file_is_killed_with_sigsys() {
    let out = worker("probe-open", "");
    assert_eq!(out.status.signal(), Some(SIGSYS), "{:?}", out);
    assert!(out.stdout.is_empty(), "worker must die before replying");
}

#[test]
fn spawning_a_process_is_killed_with_sigsys() {
    let out = worker("probe-spawn", "");
    assert_eq!(out.status.signal(), Some(SIGSYS), "{:?}", out);
}

#[test]
fn allowed_work_succeeds_inside_filter() {
    let out = worker("calc", "2 * (3 + 4)");
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], 14.0);
}

#[test]
fn run_job_reports_violation() {
    let err = mcp_server::sandbox::run_job(BIN.as_ref(), "probe-open", b"").unwrap_err();
    assert!(err.to_string().contains("SIGSYS"), "{err}");
}
