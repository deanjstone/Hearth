// Ported from electron/main/terminal/login-path.ts. GUI-launched apps (from
// a desktop launcher rather than a terminal) inherit a stunted PATH that
// often lacks the user's shell additions — so `claude`/`codex` (installed
// via Homebrew, npm-global, nvm, etc.) won't resolve in the PTY or in
// spawned CLI lookups. We resolve the user's real login PATH ONCE (a single
// login+interactive shell at first use, cached) and merge it with the
// inherited PATH. Spawning a login shell per terminal would re-source heavy
// rc files every time; once is enough.
//
// The real login-shell/`which` calls are behind a `ShellQuery` trait — the
// DI seam spec #26's testing decisions call for ("one injected trait per
// subsystem... tested with fakes, not real subprocesses"), letting
// `LoginPathResolver`'s caching/fallback logic be tested without spawning a
// real shell. `RealShellQuery` (the concrete OS-backed impl) additionally
// gets its own narrow real-subprocess tests, matching the precedent already
// set by `selfmod/validate.rs`'s `run_command_capture`.
//
// One deliberate consolidation vs the TS original: `login-path.ts` and
// `pty.ts` each independently default to `env.SHELL || "/bin/zsh"` when
// resolving which shell to use. Here, the caller (`terminal::pty`) computes
// that default once (`default_shell`) and passes it in — same fallback
// value either way, just not duplicated.

use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const QUERY_TIMEOUT: Duration = Duration::from_secs(4);

/// Shells out for the two OS queries this module needs: resolving a login
/// shell's PATH, and checking whether a CLI resolves against a given env.
pub trait ShellQuery: Send + Sync {
    /// Runs `<shell> -lic 'printf ...'` (or the platform equivalent) and
    /// returns the resolved PATH, or `None` on any failure/timeout.
    fn resolve_login_path(&self, shell: &str) -> Option<String>;
    /// `which <name>` (`where` on Windows) against the given env. True iff
    /// it resolves to something.
    fn which(&self, name: &str, env: &HashMap<String, String>) -> bool;
}

/// The real OS-backed `ShellQuery`.
pub struct RealShellQuery;

impl ShellQuery for RealShellQuery {
    fn resolve_login_path(&self, shell: &str) -> Option<String> {
        // Wrap PATH in sentinels so rc-file noise (banners, prompts) can't
        // corrupt it — same trick as the TS original.
        let (_, out) = run_capture_stdout(
            shell,
            &["-lic", r#"printf "__HP__%s__HP__" "$PATH""#],
            None,
            QUERY_TIMEOUT,
        )?;
        extract_sentinel(&out)
    }

    fn which(&self, name: &str, env: &HashMap<String, String>) -> bool {
        let finder = if cfg!(target_os = "windows") {
            "where"
        } else {
            "which"
        };
        match run_capture_stdout(finder, &[name], Some(env), QUERY_TIMEOUT) {
            Some((true, out)) => !out.trim().is_empty(),
            _ => false,
        }
    }
}

/// Pull the value wrapped between two `__HP__` sentinels out of `s`. `None`
/// if the sentinels aren't both present or the wrapped value is blank.
fn extract_sentinel(s: &str) -> Option<String> {
    const SENTINEL: &str = "__HP__";
    let start = s.find(SENTINEL)? + SENTINEL.len();
    let rest = &s[start..];
    let end = rest.find(SENTINEL)?;
    let val = rest[..end].trim();
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

/// Spawn `program` with `args` (and, if given, a replacement env), capture
/// stdout, and kill it if it outlives `timeout`. Returns `(exited_ok,
/// stdout)`, or `None` on spawn failure. Mirrors `selfmod/validate.rs`'s
/// `run_command_capture` poll-loop shape, simplified: no process-group kill
/// (neither a login shell nor `which` forks a subtree worth chasing) and
/// stdout-only (no stderr capture needed by either caller here).
fn run_capture_stdout(
    program: &str,
    args: &[&str],
    env: Option<&HashMap<String, String>>,
    timeout: Duration,
) -> Option<(bool, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    if let Some(env) = env {
        cmd.env_clear();
        cmd.envs(env);
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break None,
        }
    };

    let buf = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    let out = String::from_utf8_lossy(&buf).into_owned();
    Some((status.map(|s| s.success()).unwrap_or(false), out))
}

/// Login PATH first (richer), then any inherited entries not already
/// present. Pure — tested directly without a shell. The Windows delimiter
/// branch is unreachable on this port's WSL2/Linux-only target (spec #26)
/// but kept for line-for-line fidelity with the TS original, matching the
/// precedent set in Chunk 4's `startup_check.rs`.
pub fn merge_paths(login: &str, inherited: &str) -> String {
    let delimiter = if cfg!(target_os = "windows") {
        ';'
    } else {
        ':'
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let combined = format!("{login}{delimiter}{inherited}");
    for p in combined.split(delimiter) {
        if !p.is_empty() && seen.insert(p) {
            out.push(p);
        }
    }
    out.join(&delimiter.to_string())
}

/// Resolves and caches the user's login PATH, and derives env/CLI-resolution
/// helpers from it. One instance lives for the app's lifetime (constructed
/// once in `lib.rs`'s `setup()`, alongside `TerminalManager`).
pub struct LoginPathResolver<Q: ShellQuery> {
    query: Q,
    is_windows: bool,
    cached: Mutex<Option<String>>,
}

impl<Q: ShellQuery> LoginPathResolver<Q> {
    pub fn new(query: Q, is_windows: bool) -> Self {
        Self {
            query,
            is_windows,
            cached: Mutex::new(None),
        }
    }

    /// The user's login PATH, resolved once and cached. Falls back to the
    /// inherited PATH if resolution fails, yields nothing, or on Windows (no
    /// login-shell concept).
    pub fn login_path(&self, inherited: &str, shell: &str) -> String {
        let mut cached = self.cached.lock().expect("login path cache mutex poisoned");
        if let Some(c) = cached.as_ref() {
            return c.clone();
        }
        let resolved = if self.is_windows {
            inherited.to_string()
        } else {
            match self.query.resolve_login_path(shell) {
                Some(login) => merge_paths(&login, inherited),
                None => inherited.to_string(),
            }
        };
        *cached = Some(resolved.clone());
        resolved
    }

    /// `base` with PATH replaced by the resolved login PATH — the env a
    /// spawned shell/CLI should actually run with.
    pub fn login_env(
        &self,
        base: &HashMap<String, String>,
        shell: &str,
    ) -> HashMap<String, String> {
        let inherited = base.get("PATH").cloned().unwrap_or_default();
        let mut out = base.clone();
        out.insert("PATH".to_string(), self.login_path(&inherited, shell));
        out
    }

    /// Whether `name` resolves on the (merged) login PATH — drives
    /// detect-and-hint in the connectors UI when `claude`/`codex` aren't
    /// installed/visible. Called from `mcp::active_connectors` (Phase 5) —
    /// this mirrors `login-path.ts`'s own file boundary, which shares this
    /// function between the terminal and the connectors surface.
    pub fn cli_resolves(&self, name: &str, base: &HashMap<String, String>, shell: &str) -> bool {
        let env = self.login_env(base, shell);
        self.query.which(name, &env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // --- merge_paths ---

    #[test]
    fn merge_paths_puts_login_entries_first_in_order() {
        assert_eq!(merge_paths("/a:/b", "/c:/d"), "/a:/b:/c:/d");
    }

    #[test]
    fn merge_paths_drops_inherited_entries_already_in_login() {
        assert_eq!(merge_paths("/a:/b", "/b:/c"), "/a:/b:/c");
    }

    #[test]
    fn merge_paths_filters_empty_segments() {
        assert_eq!(merge_paths("/a::/b", ":/c:"), "/a:/b:/c");
    }

    #[test]
    fn merge_paths_dedups_within_login_itself() {
        assert_eq!(merge_paths("/a:/a", "/b"), "/a:/b");
    }

    // --- extract_sentinel ---

    #[test]
    fn extract_sentinel_pulls_the_wrapped_value() {
        assert_eq!(
            extract_sentinel("__HP__/usr/bin:/bin__HP__"),
            Some("/usr/bin:/bin".to_string())
        );
    }

    #[test]
    fn extract_sentinel_ignores_banner_noise_around_it() {
        assert_eq!(
            extract_sentinel("Welcome!\n__HP__/usr/bin__HP__\n"),
            Some("/usr/bin".to_string())
        );
    }

    #[test]
    fn extract_sentinel_none_when_sentinels_missing() {
        assert_eq!(extract_sentinel("no sentinels here"), None);
    }

    #[test]
    fn extract_sentinel_none_when_wrapped_value_is_blank() {
        assert_eq!(extract_sentinel("__HP__   __HP__"), None);
    }

    // --- LoginPathResolver, against a fake ShellQuery ---

    struct FakeShellQuery {
        resolved: Option<String>,
        resolve_calls: AtomicUsize,
        which_result: bool,
    }

    impl FakeShellQuery {
        fn returning(resolved: Option<&str>) -> Self {
            Self {
                resolved: resolved.map(str::to_string),
                resolve_calls: AtomicUsize::new(0),
                which_result: false,
            }
        }
    }

    impl ShellQuery for FakeShellQuery {
        fn resolve_login_path(&self, _shell: &str) -> Option<String> {
            self.resolve_calls.fetch_add(1, Ordering::SeqCst);
            self.resolved.clone()
        }
        fn which(&self, _name: &str, _env: &HashMap<String, String>) -> bool {
            self.which_result
        }
    }

    #[test]
    fn login_path_merges_resolved_login_path_with_inherited() {
        let resolver =
            LoginPathResolver::new(FakeShellQuery::returning(Some("/opt/homebrew/bin")), false);
        assert_eq!(
            resolver.login_path("/usr/bin", "/bin/zsh"),
            "/opt/homebrew/bin:/usr/bin"
        );
    }

    #[test]
    fn login_path_falls_back_to_inherited_when_query_fails() {
        let resolver = LoginPathResolver::new(FakeShellQuery::returning(None), false);
        assert_eq!(resolver.login_path("/usr/bin", "/bin/zsh"), "/usr/bin");
    }

    #[test]
    fn login_path_on_windows_never_queries_and_returns_inherited() {
        let query = FakeShellQuery::returning(Some("/opt/homebrew/bin"));
        let resolver = LoginPathResolver::new(query, true);
        assert_eq!(
            resolver.login_path("C:\\Windows", "powershell.exe"),
            "C:\\Windows"
        );
        assert_eq!(resolver.query.resolve_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn login_path_caches_after_first_resolution() {
        let resolver = LoginPathResolver::new(FakeShellQuery::returning(Some("/opt/bin")), false);
        let first = resolver.login_path("/usr/bin", "/bin/zsh");
        let second = resolver.login_path("/usr/bin", "/bin/zsh");
        assert_eq!(first, second);
        assert_eq!(resolver.query.resolve_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn login_env_replaces_path_and_keeps_other_vars() {
        let resolver = LoginPathResolver::new(FakeShellQuery::returning(Some("/opt/bin")), false);
        let mut base = HashMap::new();
        base.insert("PATH".to_string(), "/usr/bin".to_string());
        base.insert("TERM".to_string(), "xterm-256color".to_string());
        let env = resolver.login_env(&base, "/bin/zsh");
        assert_eq!(env.get("PATH").unwrap(), "/opt/bin:/usr/bin");
        assert_eq!(env.get("TERM").unwrap(), "xterm-256color");
    }

    #[test]
    fn cli_resolves_delegates_to_the_query_with_the_login_env() {
        let mut query = FakeShellQuery::returning(Some("/opt/bin"));
        query.which_result = true;
        let resolver = LoginPathResolver::new(query, false);
        assert!(resolver.cli_resolves("claude", &HashMap::new(), "/bin/zsh"));
    }

    // --- RealShellQuery, against real (small, portable) subprocesses ---

    #[test]
    fn real_which_finds_a_binary_that_exists_on_path() {
        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        );
        assert!(RealShellQuery.which("sh", &env));
    }

    #[test]
    fn real_which_reports_false_for_a_binary_that_does_not_exist() {
        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        );
        assert!(!RealShellQuery.which("definitely-not-a-real-binary-xyz", &env));
    }

    #[test]
    fn real_resolve_login_path_does_not_hang_and_never_returns_a_blank_value() {
        // A real end-to-end sanity check of the spawn + sentinel-extraction
        // plumbing (extract_sentinel's own logic is covered directly above).
        // Not asserting `is_some()`: `sh -lic` is an *interactive* login
        // shell, and a minimal/headless CI runner's `sh` can legitimately
        // decline to run interactively without a controlling tty — that's a
        // environment property, not a bug in this code. What must always
        // hold is "no hang, and never a blank/whitespace-only Some".
        let start = Instant::now();
        let resolved = RealShellQuery.resolve_login_path("sh");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "should resolve well within the query timeout"
        );
        if let Some(path) = resolved {
            assert!(!path.trim().is_empty());
        }
    }

    #[test]
    fn real_query_timeout_does_not_hang_forever() {
        let start = Instant::now();
        let result =
            run_capture_stdout("sh", &["-c", "sleep 30"], None, Duration::from_millis(200));
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "kill should cut sleep short"
        );
        // Killed before printing anything meaningful to stdout.
        assert!(result.is_none() || !result.unwrap().0);
    }
}
