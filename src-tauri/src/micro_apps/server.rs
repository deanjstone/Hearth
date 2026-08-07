// Ported from electron/main/micro-apps/server.ts (Phase 6, tracking issue
// #27). One Vite dev server per micro-app, run from the app's OWN
// node_modules so we don't depend on a global install, installing deps on
// first start if they're missing. A micro-app is its own isolated Vite +
// React project with its own deps.
//
// Unlike the TS original, the URL this returns is never handed straight to
// a renderer: a `csp_proxy::CspProxy` (micro_apps_commands.rs's orchestrator)
// sits in front of it, so `microAppForOrigin` — needed there to map an
// arbitrary session-wide request back to its owning app — has no equivalent
// here. Each proxy instance already knows which app it serves at
// construction time.

use crate::micro_apps::validate::assert_app_name;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;

const START_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct MicroAppInfo {
    pub name: String,
    pub running: bool,
}

struct RunningApp {
    child: Child,
    /// Vite's own loopback URL — internal only; see the module comment for
    /// why callers get a proxy URL instead.
    vite_url: String,
}

/// Vite prints something like "  ➜  Local: http://localhost:5173/". Match
/// the first loopback URL in a chunk, tolerating surrounding text and ANSI
/// codes. Pure, unit-tested; hand-rolled rather than pulling in `regex`
/// (fancy-regex is a dependency, but for backtracking features this simple
/// anchored-literal scan doesn't need).
pub fn extract_dev_url(chunk: &str) -> Option<String> {
    const PREFIXES: [&str; 4] = [
        "http://localhost:",
        "https://localhost:",
        "http://127.0.0.1:",
        "https://127.0.0.1:",
    ];
    let mut best: Option<(usize, usize)> = None; // (start byte, prefix len)
    for prefix in PREFIXES {
        if let Some(start) = chunk.find(prefix) {
            if best.is_none_or(|(b, _)| start < b) {
                best = Some((start, prefix.len()));
            }
        }
    }
    let (start, prefix_len) = best?;
    let after = start + prefix_len;
    let digits_end = chunk[after..]
        .find(|c: char| !c.is_ascii_digit())
        .map(|i| after + i)
        .unwrap_or(chunk.len());
    if digits_end == after {
        return None; // no port digits right after the colon
    }
    let end = if chunk[digits_end..].starts_with('/') {
        digits_end + 1
    } else {
        digits_end
    };
    Some(chunk[start..end].to_string())
}

/// Lexically collapse `.`/`..` without touching the filesystem — same
/// purpose as terminal_commands.rs's own copy (kept local rather than
/// shared: both are small, self-contained, and used for a different check).
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn micro_app_dir(repo_root: &Path, name: &str) -> Result<PathBuf, String> {
    let apps_dir = repo_root.join("micro-apps");
    let dir = apps_dir.join(name);
    // Defense in depth: even a regex-passing name must resolve inside
    // micro-apps/ (mirrors validate.rs's own comment).
    if !normalize_lexically(&dir).starts_with(normalize_lexically(&apps_dir)) {
        return Err(format!("Invalid app name: {name}"));
    }
    Ok(dir)
}

fn vite_bin(dir: &Path) -> PathBuf {
    dir.join("node_modules").join(".bin").join("vite")
}

/// Install the micro-app's deps with pnpm. `--ignore-scripts` is a hard
/// security boundary: a micro-app's package.json is agent-authored and
/// therefore untrusted, and lifecycle scripts (postinstall et al.) run with
/// full process privileges. Vite + React need no install scripts to
/// function.
///
/// `--ignore-workspace`: `dir` (micro-apps/<name>) is deliberately NOT a
/// member of this repo's own pnpm-workspace.yaml (each micro-app is its own
/// isolated project, per this module's own header comment) — but it's still
/// nested inside the repo's directory tree, so pnpm auto-detects the parent
/// workspace root and, without this flag, treats the whole command as "the
/// parent workspace is already satisfied" and silently does nothing for
/// this subdirectory's own package.json (confirmed empirically: exits 0,
/// prints "Already up to date", never creates this dir's own node_modules).
/// Found via e2e-tests/specs/micro-app-csp-proxy.spec.js, the first thing to
/// actually run this against a real micro-app nested in the real workspace
/// rather than a `tempfile::tempdir()` fixture living outside it.
pub async fn install_deps(dir: &Path) -> Result<(), String> {
    let output = Command::new("pnpm")
        .args(["install", "--ignore-scripts", "--ignore-workspace"])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("pnpm install failed to spawn in {}: {e}", dir.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    let suffix = if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    };
    Err(format!(
        "pnpm install failed in {} ({}){suffix}",
        dir.display(),
        exit_desc(&output.status)
    ))
}

fn exit_desc(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit {code}"),
        None => "terminated by signal".to_string(),
    }
}

/// Tracks running micro-app Vite dev servers, keyed by name. One instance is
/// managed for the whole app's lifetime (see micro_apps_commands.rs).
pub struct MicroAppServer {
    running: Mutex<HashMap<String, RunningApp>>,
}

impl Default for MicroAppServer {
    fn default() -> Self {
        Self::new()
    }
}

impl MicroAppServer {
    pub fn new() -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Vite's own URL for an already-running app, or `None`.
    pub fn vite_url(&self, name: &str) -> Option<String> {
        self.running
            .lock()
            .unwrap()
            .get(name)
            .map(|a| a.vite_url.clone())
    }

    /// Drop any tracked app whose child has actually exited — `server.ts`
    /// registered `child.on('exit', () => running.delete(name))` for this;
    /// `tokio::process::Child` has no exit callback, only a poll
    /// (`try_wait`), so this is called from every read/start path instead of
    /// once at spawn time. Without it, a Vite that crashes AFTER startup
    /// (not during the read_loop below, which already handles that case)
    /// leaves a stale entry forever: `list()` reports `running: true` for a
    /// dead app, and `ensure_started` would keep handing out a URL fronting
    /// nothing.
    fn reap_dead(&self) {
        let mut running = self.running.lock().unwrap();
        running.retain(|_, app| !matches!(app.child.try_wait(), Ok(Some(_))));
    }

    /// Start (or reuse) `name`'s Vite dev server, returning its own loopback
    /// URL. Idempotent: a second call while already running returns the
    /// existing URL without spawning anything.
    pub async fn ensure_started(&self, repo_root: &Path, name: &str) -> Result<String, String> {
        let name = assert_app_name(name)?;
        self.reap_dead();
        if let Some(url) = self.vite_url(&name) {
            return Ok(url);
        }

        let dir = micro_app_dir(repo_root, &name)?;
        if !dir.exists() {
            return Err(format!("micro-app not found: {}", dir.display()));
        }

        let vite = vite_bin(&dir);
        if !vite.exists() {
            install_deps(&dir).await?;
            if !vite.exists() {
                return Err(format!(
                    "micro-app {name} has no vite after install (check its package.json)"
                ));
            }
        }

        let mut child = Command::new(&vite)
            .arg("--strictPort=false")
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn vite for {name}: {e}"))?;

        let stdout = child.stdout.take().expect("vite spawned with piped stdout");
        let mut lines = BufReader::new(stdout).lines();

        // Captures every stdout/stderr line seen during startup (capped, so
        // a runaway process can't grow this unboundedly) so a failure —
        // timeout or otherwise — can report what vite actually said instead
        // of a bare "no URL". Diagnostic-only: not used for anything but
        // building the two error messages below. `eprintln!` also still
        // goes to Hearth's own process stderr for a developer tailing it
        // directly, but that stream isn't visible from e2e-tests/'s own CI
        // logs (a separate process tauri-driver launches), which is why the
        // capture below — surfaced through this fn's own `Err` — is what
        // actually reaches a CI failure's log.
        const CAPTURE_LIMIT: usize = 40;
        let captured: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();

        // Drain stderr for the process's whole lifetime, not just while
        // waiting for the URL. `Stdio::piped()` gives it a fixed-size OS
        // pipe buffer (~64KB on Linux) — if nothing ever reads it and vite
        // writes enough (deprecation/engine-version warnings, more of them
        // observed in CI than a typical local run), the write() blocks and
        // vite hangs before ever printing its "Local:" URL to stdout. Found
        // via e2e-tests/specs/micro-app-csp-proxy.spec.js failing in CI
        // with "did not print a dev URL within 30s" while the same fixture
        // started in under a second locally.
        let stderr = child.stderr.take().expect("vite spawned with piped stderr");
        let stderr_app_name = name.clone();
        let stderr_captured = captured.clone();
        tokio::spawn(async move {
            let mut stderr_lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                eprintln!("[hearth] micro-app {stderr_app_name} (vite stderr): {line}");
                let mut buf = stderr_captured.lock().unwrap();
                if buf.len() >= CAPTURE_LIMIT {
                    buf.remove(0);
                }
                buf.push(format!("stderr: {line}"));
            }
        });

        let read_loop = async {
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        {
                            let mut buf = captured.lock().unwrap();
                            if buf.len() >= CAPTURE_LIMIT {
                                buf.remove(0);
                            }
                            buf.push(format!("stdout: {line}"));
                        }
                        if let Some(url) = extract_dev_url(&line) {
                            return Ok(url);
                        }
                    }
                    Ok(None) => {
                        let status = child.wait().await.ok();
                        let desc = status
                            .as_ref()
                            .map(exit_desc)
                            .unwrap_or_else(|| "unknown exit".to_string());
                        let snapshot = captured.lock().unwrap().join("\n");
                        let detail = if snapshot.is_empty() {
                            String::new()
                        } else {
                            format!(":\n{snapshot}")
                        };
                        return Err(format!(
                            "micro-app {name} vite exited ({desc}) before printing a URL{detail}"
                        ));
                    }
                    Err(e) => return Err(format!("micro-app {name} vite stdout error: {e}")),
                }
            }
        };

        let url = match timeout(START_TIMEOUT, read_loop).await {
            Ok(Ok(url)) => url,
            Ok(Err(e)) => {
                let _ = child.start_kill();
                return Err(e);
            }
            Err(_elapsed) => {
                let _ = child.start_kill();
                let snapshot = captured.lock().unwrap().join("\n");
                let detail = if snapshot.is_empty() {
                    " (no output captured)".to_string()
                } else {
                    format!(":\n{snapshot}")
                };
                return Err(format!(
                    "micro-app {name} did not print a dev URL within {}s{detail}",
                    START_TIMEOUT.as_secs()
                ));
            }
        };

        self.running.lock().unwrap().insert(
            name,
            RunningApp {
                child,
                vite_url: url.clone(),
            },
        );
        Ok(url)
    }

    pub fn stop(&self, name: &str) {
        if let Some(mut app) = self.running.lock().unwrap().remove(name) {
            let _ = app.child.start_kill();
        }
    }

    pub fn stop_all(&self) {
        let mut running = self.running.lock().unwrap();
        for (_, mut app) in running.drain() {
            let _ = app.child.start_kill();
        }
    }

    fn is_running(&self, name: &str) -> bool {
        self.running.lock().unwrap().contains_key(name)
    }

    /// List the scaffolded micro-apps (dirs under micro-apps/ that look like
    /// a project), with whether each currently has a running dev server.
    /// Powers the Tools gallery.
    pub fn list(&self, repo_root: &Path) -> Vec<MicroAppInfo> {
        self.reap_dead();
        let apps_dir = repo_root.join("micro-apps");
        let Ok(entries) = std::fs::read_dir(&apps_dir) else {
            return Vec::new();
        };
        let mut out: Vec<MicroAppInfo> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir() && e.path().join("package.json").exists())
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let running = self.is_running(&name);
                MicroAppInfo { name, running }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn extract_dev_url_matches_a_plain_localhost_url_with_trailing_slash() {
        assert_eq!(
            extract_dev_url("http://localhost:5173/"),
            Some("http://localhost:5173/".to_string())
        );
    }

    #[test]
    fn extract_dev_url_matches_a_127_url_without_trailing_slash() {
        assert_eq!(
            extract_dev_url("http://127.0.0.1:5174"),
            Some("http://127.0.0.1:5174".to_string())
        );
    }

    #[test]
    fn extract_dev_url_matches_inside_a_typical_vite_local_line() {
        let line = "  ➜  Local:   http://localhost:5173/";
        assert_eq!(
            extract_dev_url(line),
            Some("http://localhost:5173/".to_string())
        );
    }

    #[test]
    fn extract_dev_url_matches_when_wrapped_in_ansi_color_codes() {
        let line = "  \x1b[32m➜\x1b[0m  Local: \x1b[36mhttp://localhost:5180/\x1b[0m";
        assert_eq!(
            extract_dev_url(line),
            Some("http://localhost:5180/".to_string())
        );
    }

    #[test]
    fn extract_dev_url_matches_https_as_well_as_http() {
        assert_eq!(
            extract_dev_url("  ➜  Local: https://localhost:5173/"),
            Some("https://localhost:5173/".to_string())
        );
    }

    #[test]
    fn extract_dev_url_returns_none_when_no_url_is_present() {
        assert_eq!(extract_dev_url("VITE v6.0.0  ready in 312 ms"), None);
    }

    #[test]
    fn extract_dev_url_returns_none_for_empty_input() {
        assert_eq!(extract_dev_url(""), None);
    }

    #[test]
    fn extract_dev_url_ignores_non_loopback_hosts() {
        assert_eq!(extract_dev_url("http://example.com:5173/"), None);
    }

    #[tokio::test]
    async fn ensure_started_rejects_a_traversal_name_before_spawning_anything() {
        let server = MicroAppServer::new();
        let err = server
            .ensure_started(Path::new("/repo"), "../../etc")
            .await
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("invalid app name"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn ensure_started_rejects_a_name_with_spaces() {
        let server = MicroAppServer::new();
        let err = server
            .ensure_started(Path::new("/repo"), "has spaces")
            .await
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("invalid app name"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn ensure_started_reports_a_missing_app_dir() {
        let dir = tempfile::tempdir().unwrap();
        let server = MicroAppServer::new();
        let err = server.ensure_started(dir.path(), "nope").await.unwrap_err();
        assert!(err.contains("not found"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn reap_dead_removes_an_app_whose_vite_process_exited_after_startup() {
        // A "vite" that prints its URL then exits immediately — simulates a
        // real crash-after-startup, which the read_loop in ensure_started
        // (only watching stdout during the initial URL wait) never catches.
        let dir = tempfile::tempdir().unwrap();
        let app_dir = dir.path().join("micro-apps").join("flaky");
        let bin_dir = app_dir.join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(app_dir.join("package.json"), "{}").unwrap();
        let script = bin_dir.join("vite");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'Local: http://127.0.0.1:59999/'\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let server = MicroAppServer::new();
        let url = server.ensure_started(dir.path(), "flaky").await.unwrap();
        assert_eq!(url, "http://127.0.0.1:59999/");

        // Give the already-exiting fake vite a moment to actually finish,
        // then confirm list() self-heals instead of reporting it forever.
        let mut still_running = true;
        for _ in 0..50 {
            still_running = server
                .list(dir.path())
                .iter()
                .any(|a| a.name == "flaky" && a.running);
            if !still_running {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !still_running,
            "flaky was still reported running after its process exited"
        );

        // Re-starting after the reap spawns fresh rather than reusing a
        // dead entry's (now-meaningless) cached URL.
        let restarted = server.ensure_started(dir.path(), "flaky").await.unwrap();
        assert_eq!(restarted, "http://127.0.0.1:59999/");
    }

    #[tokio::test]
    // Requires `pnpm` on PATH — true for a normal dev shell and the
    // `checks`/`e2e` CI jobs (both run `pnpm/action-setup`), but not the
    // `rust` job, which is deliberately Rust-toolchain-only. Same
    // ignore-by-default precedent as agents/acp_client.rs's own `node`-
    // dependent test.
    #[ignore]
    async fn install_deps_does_not_run_a_postinstall_script() {
        // W4: a micro-app's package.json is agent-authored and untrusted.
        // Hermetic — no dependencies means no network needed for this test.
        let dir = tempfile::tempdir().unwrap();
        let sentinel = dir.path().join("PWNED");
        std::fs::write(
            dir.path().join("package.json"),
            serde_json::json!({
                "name": "evil",
                "version": "0.0.0",
                "private": true,
                "scripts": { "postinstall": format!("touch {}", sentinel.display()) },
            })
            .to_string(),
        )
        .unwrap();

        // pnpm must be on PATH — same assumption server.rs's real callers
        // already make (this whole build is pnpm-driven).
        install_deps(dir.path()).await.unwrap();
        assert!(!sentinel.exists());
    }

    #[test]
    fn list_returns_empty_when_micro_apps_dir_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let server = MicroAppServer::new();
        assert!(server.list(dir.path()).is_empty());
    }

    #[test]
    fn list_finds_scaffolded_apps_sorted_by_name() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["zeta", "alpha"] {
            let app_dir = dir.path().join("micro-apps").join(name);
            std::fs::create_dir_all(&app_dir).unwrap();
            std::fs::write(app_dir.join("package.json"), "{}").unwrap();
        }
        // A dir with no package.json isn't a scaffolded app.
        std::fs::create_dir_all(dir.path().join("micro-apps").join("not-an-app")).unwrap();

        let server = MicroAppServer::new();
        let apps = server.list(dir.path());
        assert_eq!(
            apps,
            vec![
                MicroAppInfo {
                    name: "alpha".to_string(),
                    running: false
                },
                MicroAppInfo {
                    name: "zeta".to_string(),
                    running: false
                },
            ]
        );
    }
}
