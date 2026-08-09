// Validation gate (W5/W6). Runs the project's typecheck after a self-mod and
// reports the result. Two modes (caller's choice, not enforced here):
//   - async (W5): fire-and-forget after a renderer (src/**) edit; a failure is
//     surfaced (banner / crash surface) but never blocks the turn.
//   - blocking (W6): awaited before a process-restart-tier apply; if it fails we
//     refuse the restart so a broken main edit can't brick boot.
//
// Ported from electron/main/self-mod/validate.ts, which shells out via
// `execFile` with a `timeout` option (Node kills the child and reports an
// error). Rust's `std::process::Command` has no built-in timeout, so this port
// polls `try_wait()` against an `Instant` deadline and kills the child itself
// on expiry — functionally equivalent, just implemented by hand.
//
// The child is also spawned as its own process-group leader (`process_group`)
// so a timeout can kill the *group*, not just the immediate pid: `pnpm run
// typecheck` forks through node into `tsc`, and SIGKILLing only the top-level
// pnpm process leaves that subprocess tree — and the pipe fds it's still
// holding open — alive. Unix-only (`kill(-pgid, SIGKILL)`), matching the
// project's WSL2/Linux-only target for this fork.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub struct TypecheckResult {
    pub ok: bool,
    /// Combined stdout+stderr (truncated) when it failed, for the surface/repair prompt.
    pub output: String,
}

/// TS default is `timeoutMs = 120_000`; Rust has no default parameters, so
/// callers must pass a timeout explicitly — this constant documents the ported
/// default value.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

const MAX_OUTPUT_BYTES: usize = 8000;

/// Run `pnpm run typecheck` in the repo. Resolves with ok=false on type errors
/// or timeout.
pub fn run_typecheck(repo_root: &Path, timeout: Duration) -> TypecheckResult {
    run_command_capture(repo_root, "pnpm", &["run", "typecheck"], timeout)
}

/// Spawn `program` with `args` in `repo_root`, capture combined stdout+stderr,
/// and kill it if it outlives `timeout`. Generic over the program so it's
/// testable with portable `sh -c` commands instead of requiring `pnpm`.
fn run_command_capture(
    repo_root: &Path,
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> TypecheckResult {
    let mut child = match Command::new(program)
        .args(args)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return TypecheckResult {
                ok: false,
                output: truncate(&e.to_string()),
            }
        }
    };

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    // Drain the pipes on dedicated threads: the child's stdout/stderr buffers
    // are bounded, so a long-timeout / large-output command could deadlock if
    // we only read after it exits (or is killed).
    let (stdout_tx, stdout_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = stdout_tx.send(buf);
    });
    let (stderr_tx, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        let _ = stderr_tx.send(buf);
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    kill_process_group(child.id());
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break None,
        }
    };

    let stdout_buf = stdout_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    let stderr_buf = stderr_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    let mut combined = String::from_utf8_lossy(&stdout_buf).into_owned();
    combined.push_str(&String::from_utf8_lossy(&stderr_buf));
    let combined = combined.trim().to_string();

    match status {
        Some(status) if status.success() => TypecheckResult {
            ok: true,
            output: String::new(),
        },
        _ => TypecheckResult {
            ok: false,
            output: truncate(&combined),
        },
    }
}

/// Kill the child's entire process group, not just the immediate pid — see the
/// module doc comment for why a plain `child.kill()` isn't enough here.
///
/// This calls `kill(2)` directly (a negative pid targets the whole process
/// group) rather than shelling out to the `kill` binary: in this project's dev
/// environment, `Command::new("kill").arg(format!("-{pid}"))` was observed to
/// report success without actually delivering the signal to the group, while
/// the raw syscall does. Shelling out was never load-bearing here — this is
/// strictly more direct and avoids spawning an extra process per timeout.
fn kill_process_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

/// Byte-safe truncate to the TS `slice(0, 8000)` character budget — Rust
/// strings are UTF-8, so cutting at a fixed byte offset can land inside a
/// multi-byte codepoint; walk backward to the nearest char boundary instead.
fn truncate(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s.to_string();
    }
    let mut end = MAX_OUTPUT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_yields_ok_with_empty_output() {
        let dir = std::env::temp_dir();
        let result = run_command_capture(&dir, "sh", &["-c", "echo hello"], Duration::from_secs(5));
        assert!(result.ok);
        assert_eq!(result.output, "");
    }

    #[test]
    fn failure_captures_combined_stdout_and_stderr() {
        let dir = std::env::temp_dir();
        let result = run_command_capture(
            &dir,
            "sh",
            &["-c", "echo out; echo err 1>&2; exit 1"],
            Duration::from_secs(5),
        );
        assert!(!result.ok);
        assert!(result.output.contains("out"));
        assert!(result.output.contains("err"));
    }

    #[test]
    fn timeout_kills_the_process_and_reports_failure() {
        let dir = std::env::temp_dir();
        let start = Instant::now();
        let result =
            run_command_capture(&dir, "sh", &["-c", "sleep 30"], Duration::from_millis(200));
        assert!(!result.ok);
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "kill should cut sleep short"
        );
    }

    #[test]
    fn large_output_gets_truncated() {
        let dir = std::env::temp_dir();
        let result = run_command_capture(
            &dir,
            "sh",
            &["-c", "yes ab | head -c 20000; exit 1"],
            Duration::from_secs(5),
        );
        assert!(!result.ok);
        assert!(result.output.len() <= MAX_OUTPUT_BYTES);
    }

    #[test]
    fn spawn_failure_of_a_missing_binary_is_reported_as_not_ok() {
        let dir = std::env::temp_dir();
        let result = run_command_capture(
            &dir,
            "definitely-not-a-real-binary-xyz",
            &[],
            Duration::from_secs(5),
        );
        assert!(!result.ok);
        assert!(!result.output.is_empty());
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        // Build a string where a 2-byte UTF-8 char (é) straddles the 8000-byte
        // cut point, so a naive `&s[..8000]` would panic mid-codepoint.
        let mut s = String::new();
        for _ in 0..(MAX_OUTPUT_BYTES - 1) {
            s.push('a');
        }
        s.push('é');
        for _ in 0..100 {
            s.push('a');
        }
        let truncated = truncate(&s);
        assert!(truncated.len() <= MAX_OUTPUT_BYTES);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn short_output_is_not_truncated() {
        assert_eq!(truncate("hello"), "hello");
    }
}
