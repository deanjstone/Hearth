// Scope guard for agent writes (W7). Splits every write target into three tiers:
//
//   blocked   — never writable: system dirs, credentials/secrets, Hearth internal
//               state, device files. Ported from Stella's `isBlockedPath`.
//   protected — the safety-net island: the self-mod engine, boot watchdog, recovery
//               anchor, and the managed `.claude` hook config. Editable by the agent
//               only with explicit user approval, so it can't silently disarm its
//               own guardrails even though the rest of main is editable.
//   canvas    — everything else (the agent's free surface), including the rest of
//               electron/main-equivalent Rust code, preload, configs, deps, src/**,
//               skills, prompts.
//
// This module is part of the protected island itself. Ported from
// electron/main/self-mod/scope-guard.ts — see docs/completed-plans/SELF-MOD-HARDENING-PLAN.md
// (W7) for the original design.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeTier {
    Blocked,
    Protected,
    Canvas,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeDecision {
    pub tier: ScopeTier,
    /// Human-readable reason, present for `Blocked`/`Protected`.
    pub reason: Option<String>,
}

impl ScopeDecision {
    fn blocked(reason: &str) -> Self {
        Self {
            tier: ScopeTier::Blocked,
            reason: Some(reason.to_string()),
        }
    }
    fn protected(reason: &str) -> Self {
        Self {
            tier: ScopeTier::Protected,
            reason: Some(reason.to_string()),
        }
    }
    fn canvas() -> Self {
        Self {
            tier: ScopeTier::Canvas,
            reason: None,
        }
    }
}

// Credential / shell-init files in the user's home that must never be written.
const SENSITIVE_HOME_FILES: &[&str] = &[
    ".zshrc",
    ".bashrc",
    ".bash_profile",
    ".zprofile",
    ".profile",
    ".netrc",
    ".pgpass",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
];

// Repo-relative files that hold secrets or Hearth internal runtime state. Note
// `.hearth/personality.json` and `.hearth/memory.md` are NOT here — those are the
// agent's soul/memory canvas. Only non-editable runtime artifacts are blocked.
const BLOCKED_REPO_FILES: &[&str] = &[
    ".env",
    ".git-credentials",
    "auth.json",
    ".hearth/bridge-url",
    ".hearth/.vite-dev-url",
    ".hearth/snapshot.png",
];

// Repo-relative prefixes for the protected safety-net island.
const PROTECTED_REPO_PREFIXES: &[&str] = &["src-tauri/src/selfmod/", ".claude/"];

fn to_posix(value: &str) -> String {
    value.replace('\\', "/")
}

/// Lexical normalization (collapse `.`/`..`, no filesystem access) — the Rust
/// equivalent of Node's `path.resolve` for an already-absolute input.
fn normalize_lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

// Absolute system directory prefixes that are never writable (port of Stella's set).
fn blocked_system_prefixes() -> Vec<PathBuf> {
    let home = home_dir();
    vec![
        PathBuf::from("/etc"),
        PathBuf::from("/usr"),
        PathBuf::from("/bin"),
        PathBuf::from("/sbin"),
        PathBuf::from("/boot"),
        PathBuf::from("/sys"),
        PathBuf::from("/proc"),
        PathBuf::from("/private/etc"),
        PathBuf::from("/private/var"),
        home.join(".ssh"),
        home.join(".aws"),
        home.join(".gnupg"),
        home.join(".kube"),
        home.join(".docker"),
        home.join(".azure"),
        home.join(".config").join("gh"),
        home.join(".config").join("gcloud"),
    ]
}

// Case-fold is deliberate: macOS/Windows filesystems are case-insensitive, so
// `SRC/secret` and `src/secret` are the SAME file on those platforms — matching
// must be case-insensitive or a case-variant path would bypass a protected
// prefix. On a case-sensitive volume (Linux, this fork's only target) this only
// ever over-protects (fail-safe), never under-.
fn normalize_abs(file_path: &Path) -> String {
    let expanded = match file_path.strip_prefix("~") {
        Ok(rest) => home_dir().join(rest),
        Err(_) => file_path.to_path_buf(),
    };
    let resolved = normalize_lexical(&expanded);
    to_posix(&resolved.to_string_lossy()).to_lowercase()
}

fn matches_prefix(normalized: &str, prefix: &Path) -> bool {
    let resolved = normalize_lexical(prefix);
    let p = to_posix(&resolved.to_string_lossy())
        .to_lowercase()
        .trim_end_matches('/')
        .to_string();
    normalized == p || normalized.starts_with(&format!("{p}/"))
}

fn is_blocked_device(normalized: &str) -> bool {
    if matches!(
        normalized,
        "/dev/null" | "/dev/zero" | "/dev/random" | "/dev/urandom"
    ) {
        return true;
    }
    if normalized.starts_with("/dev/") {
        return true;
    }
    if let Some(rest) = normalized.strip_prefix("/proc/") {
        if let Some((pid, tail)) = rest.split_once("/fd/") {
            if !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) {
                return matches!(tail, "0" | "1" | "2");
            }
        }
    }
    false
}

fn is_sensitive_home_path(normalized: &str) -> bool {
    let home = home_dir();
    SENSITIVE_HOME_FILES
        .iter()
        .any(|rel| normalized == normalize_abs(&home.join(rel)))
}

/// Repo-relative posix path if `abs_path` is inside `repo_root`, else `None`.
/// Mirrors Node's `path.relative`: common-prefix diff, `..` for the remainder
/// of `repo_root`, then the rest of `abs_path`.
fn to_repo_relative(abs_path: &Path, repo_root: &Path) -> Option<String> {
    let base = normalize_lexical(repo_root);
    let target = normalize_lexical(abs_path);
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();

    let mut i = 0;
    while i < base_components.len()
        && i < target_components.len()
        && base_components[i] == target_components[i]
    {
        i += 1;
    }

    let mut rel = PathBuf::new();
    for _ in i..base_components.len() {
        rel.push("..");
    }
    for comp in &target_components[i..] {
        rel.push(comp.as_os_str());
    }

    let rel_str = to_posix(&rel.to_string_lossy());
    if rel_str.is_empty() || rel_str.starts_with("..") {
        return None;
    }
    Some(rel_str)
}

/// Classify a write target. `raw_path` may be absolute or repo-relative; if
/// repo-relative it is resolved against `repo_root`. System/secret checks run on
/// the absolute form; island/canvas checks run on the repo-relative form.
pub fn classify_write(raw_path: &str, repo_root: &Path) -> ScopeDecision {
    let raw = Path::new(raw_path);
    let abs_path = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_root.join(raw)
    };
    let normalized = normalize_abs(&abs_path);

    if is_blocked_device(&normalized) {
        return ScopeDecision::blocked("device files are not writable");
    }
    if is_sensitive_home_path(&normalized) {
        return ScopeDecision::blocked("credential / shell-init files are not writable");
    }
    for prefix in blocked_system_prefixes() {
        if matches_prefix(&normalized, &prefix) {
            return ScopeDecision::blocked("system directories are not writable");
        }
    }

    // Writes outside the repo (e.g. a user workspace) aren't self-mod; allow them
    // once the system/secret guards above have passed.
    let Some(rel) = to_repo_relative(&abs_path, repo_root) else {
        return ScopeDecision::canvas();
    };

    if rel == ".git" || rel.starts_with(".git/") {
        return ScopeDecision::blocked("the git internal dir is not writable");
    }
    if BLOCKED_REPO_FILES.contains(&rel.as_str()) || rel.starts_with(".env.") {
        return ScopeDecision::blocked("secrets / internal state are not writable");
    }
    for prefix in PROTECTED_REPO_PREFIXES {
        let trimmed = prefix.trim_end_matches('/');
        if rel == trimmed || rel.starts_with(prefix) {
            return ScopeDecision::protected("safety-net island — requires explicit approval");
        }
    }
    ScopeDecision::canvas()
}

/// Convenience: true when the path may be written without approval.
pub fn is_canvas_path(raw_path: &str, repo_root: &Path) -> bool {
    classify_write(raw_path, repo_root).tier == ScopeTier::Canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        PathBuf::from("/Users/x/Hearth")
    }

    fn abs(rel: &str) -> PathBuf {
        repo().join(rel)
    }

    #[test]
    fn canvas_paths() {
        let canvas = [
            "src/app/chat/ChatApp.tsx",
            "src/shell/Sidebar.tsx",
            "src/styles/hearth.css",
            "electron/main/index.ts",
            "electron/preload/index.ts",
            "electron.vite.config.ts",
            "package.json",
            "AGENTS.md",
            "CLAUDE.md",
            ".hearth/personality.json",
            ".hearth/memory.md",
            ".hearth/scratchpad.md",
            "micro-apps/foo/App.tsx",
        ];
        for p in canvas {
            let target = abs(p);
            let target_str = target.to_string_lossy().to_string();
            assert_eq!(
                classify_write(&target_str, &repo()).tier,
                ScopeTier::Canvas,
                "expected canvas for {p}"
            );
            assert!(
                is_canvas_path(&target_str, &repo()),
                "expected canvas for {p}"
            );
        }
    }

    #[test]
    fn protected_island_paths() {
        let protected = [
            "src-tauri/src/selfmod/scope_guard.rs",
            "src-tauri/src/selfmod/boot_watchdog.rs",
            "src-tauri/src/selfmod/recovery/anchor.rs",
            ".claude/settings.json",
            ".claude/hooks/block-source-writes.sh",
        ];
        for p in protected {
            let target = abs(p).to_string_lossy().to_string();
            assert_eq!(
                classify_write(&target, &repo()).tier,
                ScopeTier::Protected,
                "expected protected for {p}"
            );
        }
    }

    #[test]
    fn repo_secrets_and_internal_state_blocked() {
        for p in [".env", ".env.local", "auth.json", ".hearth/bridge-url"] {
            let target = abs(p).to_string_lossy().to_string();
            assert_eq!(
                classify_write(&target, &repo()).tier,
                ScopeTier::Blocked,
                "{p}"
            );
        }
        let git_config = abs(".git/config").to_string_lossy().to_string();
        assert_eq!(
            classify_write(&git_config, &repo()).tier,
            ScopeTier::Blocked
        );
    }

    #[test]
    fn system_directories_blocked() {
        assert_eq!(
            classify_write("/etc/hosts", &repo()).tier,
            ScopeTier::Blocked
        );
        assert_eq!(
            classify_write("/usr/local/bin/x", &repo()).tier,
            ScopeTier::Blocked
        );
        assert_eq!(
            classify_write("/dev/null", &repo()).tier,
            ScopeTier::Blocked
        );
    }

    #[test]
    fn home_credential_files_blocked() {
        let home = home_dir();
        for p in [".ssh/id_rsa", ".zshrc", ".git-credentials"] {
            let target = home.join(p).to_string_lossy().to_string();
            assert_eq!(
                classify_write(&target, &repo()).tier,
                ScopeTier::Blocked,
                "{p}"
            );
        }
    }

    #[test]
    fn outside_repo_is_canvas() {
        assert_eq!(
            classify_write("/Users/x/projects/other/src/main.ts", &repo()).tier,
            ScopeTier::Canvas
        );
    }
}
