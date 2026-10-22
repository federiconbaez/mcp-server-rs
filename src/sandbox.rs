//! Seccomp sandbox for untrusted tool work (Linux only).
//!
//! The server never evaluates tool input in its own process. Instead it
//! re-executes its own binary as a short-lived worker
//! (`mcp-server-rs __sandbox-worker <job>`). The worker:
//!
//! 1. lowers its resource limits (CPU time, address space, file size,
//!    open files, processes);
//! 2. sets `PR_SET_NO_NEW_PRIVS` and installs a seccomp-BPF allow-list that
//!    permits only reading stdin, writing stdout, memory management and exit
//!    (anything else kills the process with `SIGSYS`);
//! 3. reads the job input from stdin, computes, writes one JSON line and
//!    exits.
//!
//! The parent clears the environment, sets `cwd` to `/`, enforces a
//! wall-clock timeout and caps the size of what it reads back.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};

/// Argument that switches the binary into worker mode.
pub const WORKER_ARG: &str = "__sandbox-worker";
/// Wall-clock budget for one sandboxed job.
pub const TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum number of bytes accepted back from the worker.
pub const MAX_OUTPUT: usize = 64 * 1024;

/// Outcome of a sandboxed job as seen by the parent.
#[derive(Debug)]
pub enum SandboxError {
    Spawn(String),
    Timeout,
    /// Worker was killed by a signal (e.g. 31 = SIGSYS on a seccomp violation).
    Signaled(i32),
    Failed(String),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxError::Spawn(e) => write!(f, "could not start sandbox worker: {e}"),
            SandboxError::Timeout => write!(f, "sandbox worker exceeded {TIMEOUT:?}"),
            SandboxError::Signaled(31) => {
                write!(f, "sandbox worker killed by SIGSYS (seccomp violation)")
            }
            SandboxError::Signaled(s) => write!(f, "sandbox worker killed by signal {s}"),
            SandboxError::Failed(e) => write!(f, "sandbox worker failed: {e}"),
        }
    }
}

/// Run `job` in a sandboxed child process built from `worker_exe`.
/// Returns the worker's JSON reply.
pub fn run_job(worker_exe: &Path, job: &str, input: &[u8]) -> Result<Value, SandboxError> {
    use std::os::unix::process::ExitStatusExt;

    let mut child = Command::new(worker_exe)
        .arg(WORKER_ARG)
        .arg(job)
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| SandboxError::Spawn(e.to_string()))?;

    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        // A worker that dies early closes the pipe; that is reported below.
        let _ = stdin.write_all(input);
    }

    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut buf);
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SandboxError::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(SandboxError::Failed(e.to_string())),
        }
    };
    let out = reader.join().unwrap_or_default();

    if let Some(sig) = status.signal() {
        return Err(SandboxError::Signaled(sig));
    }
    if !status.success() {
        return Err(SandboxError::Failed(format!("exit status {status}")));
    }
    if out.len() > MAX_OUTPUT {
        return Err(SandboxError::Failed("output too large".into()));
    }
    serde_json::from_slice(&out).map_err(|e| SandboxError::Failed(format!("bad output: {e}")))
}

/// Syscalls the worker may use once the filter is installed.
fn allowed_syscalls() -> Vec<i64> {
    vec![
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_brk,
        libc::SYS_mmap,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_futex,
        libc::SYS_sigaltstack,
        libc::SYS_rt_sigreturn,
        libc::SYS_rt_sigprocmask,
    ]
}

fn build_filter() -> Result<BpfProgram, String> {
    let arch = std::env::consts::ARCH
        .try_into()
        .map_err(|e| format!("unsupported arch: {e:?}"))?;
    let rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = allowed_syscalls()
        .into_iter()
        .map(|nr| (nr, Vec::new()))
        .collect();
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
        arch,
    )
    .map_err(|e| e.to_string())?;
    filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| e.to_string())
}

fn set_limit(res: libc::__rlimit_resource_t, val: libc::rlim_t) -> Result<(), String> {
    let lim = libc::rlimit {
        rlim_cur: val,
        rlim_max: val,
    };
    // SAFETY: setrlimit only reads the struct we pass by pointer.
    if unsafe { libc::setrlimit(res, &lim) } != 0 {
        return Err(format!(
            "setrlimit({res}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Lower rlimits and install the seccomp filter in the current process.
/// Irreversible: call only from the worker.
pub fn enter_sandbox() -> Result<(), String> {
    set_limit(libc::RLIMIT_CPU, 2)?;
    set_limit(libc::RLIMIT_AS, 256 * 1024 * 1024)?;
    set_limit(libc::RLIMIT_FSIZE, 0)?;
    set_limit(libc::RLIMIT_NPROC, 0)?;
    set_limit(libc::RLIMIT_NOFILE, 4)?;
    let prog = build_filter()?;
    seccompiler::apply_filter(&prog).map_err(|e| e.to_string())
}

/// Entry point of worker mode. Returns the process exit code.
pub fn worker_main(job: &str) -> i32 {
    // Read the whole input before sandboxing would also be fine, but reading
    // after proves that `read(2)` on stdin is all the worker needs.
    if let Err(e) = enter_sandbox() {
        let _ = writeln!(std::io::stdout(), "{}", json!({ "err": e }));
        return 2;
    }
    let mut input = Vec::new();
    if std::io::stdin()
        .take(crate::calc::MAX_INPUT as u64 + 1)
        .read_to_end(&mut input)
        .is_err()
    {
        return 3;
    }
    let reply = match job {
        "calc" => match std::str::from_utf8(&input)
            .map_err(|_| "input is not UTF-8".to_string())
            .and_then(crate::calc::eval)
        {
            Ok(v) => json!({ "ok": v }),
            Err(e) => json!({ "err": e }),
        },
        // Deliberately violates the filter: used by tests to prove the
        // sandbox is active. Must die with SIGSYS before printing.
        "probe-open" => {
            let r = std::fs::File::open("/etc/hostname");
            json!({ "escaped": r.is_ok() })
        }
        // Deliberately tries to spawn a process.
        "probe-spawn" => {
            let r = Command::new("/bin/true").status();
            json!({ "escaped": r.is_ok() })
        }
        other => json!({ "err": format!("unknown job '{other}'") }),
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{reply}");
    let _ = out.flush();
    0
}
