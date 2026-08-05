// Claude Code backend adapter resolution — spawns @zed-industries/claude-agent-acp,
// which vendors the Claude Code CLI and exposes it over ACP. Ported from
// electron/main/agents/claude.ts.
//
// Auth: subscription-only this phase (spec #48's Out of Scope — no api-key
// path). We inherit the user's existing `claude login` by spawning in their
// environment; Hearth never originates or stores a credential.

use super::acp_client::{resolve_adapter_bin, AcpClient, AdapterSpec};
use super::agent::{AgentConfig, AgentKind};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The vendored adapter package + bin name, shared with `startup_check.rs` so
/// the eager availability check can't drift from what actually gets spawned.
pub const PACKAGE: &str = "@zed-industries/claude-agent-acp";
pub const BIN: &str = "claude-agent-acp";

// Permission mode is driven at RUNTIME over ACP (`AgentSession::set_mode`,
// applied per session by `AgentHostEngine` — both backends start at the
// Default/prompt baseline). The static settings-file pin Hearth used to write
// is gone.
//
// BUT: the bundled claude-agent-acp (0.23.1) resolves `permissions.defaultMode`
// from the merged CLI settings at *every* `session/new` and HARD-CRASHES on a
// value it can't parse (e.g. `auto`, which newer Claude CLIs accept but this
// adapter does not). A user with `defaultMode: "auto"` in ~/.claude/settings.json
// would break session creation before runtime mode control ever runs. So we
// keep one narrow compatibility shim: only when the user's *effective* merged
// defaultMode is unparseable do we write a parseable baseline (`default`) into
// the project's settings.local.json (highest precedence) to shield the
// adapter. We never touch a valid user setting, never write other keys, and
// never clobber hooks/allow/etc. (the adapter loads hooks from these files).
const ADAPTER_PARSEABLE_MODES: &[&str] = &[
    "default",
    "acceptedits",
    "dontask",
    "plan",
    "bypasspermissions",
    "bypass",
];

fn read_default_mode(file: &Path) -> Option<serde_json::Value> {
    let content = fs::read_to_string(file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("permissions")?.get("defaultMode").cloned()
}

fn is_parseable_mode(v: Option<&serde_json::Value>) -> bool {
    match v {
        None => true,
        Some(serde_json::Value::String(s)) => {
            ADAPTER_PARSEABLE_MODES.contains(&s.trim().to_lowercase().as_str())
        }
        Some(_) => false,
    }
}

/// Shield the adapter from an unparseable merged `defaultMode` (e.g. the
/// user's global `auto`). No-op when the effective value is already valid or
/// absent. Ported from `ensureParseablePermissionMode` (claude.ts). `home` is
/// parameterized (like `login_presence.rs`'s `has_stored_login`) so this is
/// testable against a fixture directory instead of the real one.
fn ensure_parseable_permission_mode(cwd: &Path, home: &Path) -> std::io::Result<()> {
    let local_file = cwd.join(".claude").join("settings.local.json");
    // Adapter merge precedence (settingSources user < project < local): last defined wins.
    let local = read_default_mode(&local_file);
    let project = read_default_mode(&cwd.join(".claude").join("settings.json"));
    let user = read_default_mode(&home.join(".claude").join("settings.json"));
    let effective = local.or(project).or(user);
    if is_parseable_mode(effective.as_ref()) {
        return Ok(()); // adapter can handle it (or it's absent -> "default")
    }

    // Write a parseable baseline into local settings (highest precedence),
    // merging surgically so every other key the user owns is preserved.
    let dir = cwd.join(".claude");
    fs::create_dir_all(&dir)?;
    let mut current: serde_json::Value = fs::read_to_string(&local_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let obj = current
        .as_object_mut()
        .expect("filtered to an object above");
    let permissions = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    if !permissions.is_object() {
        *permissions = serde_json::json!({});
    }
    permissions
        .as_object_mut()
        .expect("normalized to an object above")
        .insert("defaultMode".to_string(), serde_json::json!("default"));
    let mut serialized = serde_json::to_string_pretty(&current).map_err(std::io::Error::other)?;
    serialized.push('\n');
    fs::write(&local_file, serialized)?;
    eprintln!(
        "[hearth] shielded adapter from unparseable permissions.defaultMode (set local baseline \"default\")"
    );
    Ok(())
}

fn resolve_adapter(config: &AgentConfig, repo_root: &Path) -> Result<AdapterSpec, String> {
    // Run the vendored claude-agent-acp bin, not whatever `claude` is on PATH.
    let bin = resolve_adapter_bin(repo_root, PACKAGE, BIN)?;

    // Mode is driven at runtime over ACP; this only shields the adapter from
    // an unparseable user defaultMode (no-op otherwise). Everything else
    // resolves from the user's normal ~/.claude config dir, so login works as
    // it does for `claude`.
    let home = dirs::home_dir().unwrap_or_default();
    ensure_parseable_permission_mode(Path::new(&config.cwd), &home).map_err(|e| {
        format!("failed to shield claude-agent-acp from an unparseable permission mode: {e}")
    })?;

    Ok(AdapterSpec {
        command: "node".to_string(),
        args: vec![bin.to_string_lossy().into_owned()],
        cwd: config.cwd.clone(),
        env: HashMap::new(),
    })
}

/// Build the Claude backend's `AcpClient`. `repo_root` resolves the vendored
/// adapter package's bin; `config` supplies the task cwd and (subscription-only,
/// this phase) auth mode.
pub fn new_agent(config: AgentConfig, repo_root: PathBuf) -> AcpClient {
    AcpClient::new(AgentKind::Claude, move || {
        resolve_adapter(&config, &repo_root)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn is_parseable_mode_accepts_absence_and_known_modes_case_insensitively() {
        assert!(is_parseable_mode(None));
        assert!(is_parseable_mode(Some(&serde_json::json!("default"))));
        assert!(is_parseable_mode(Some(&serde_json::json!(
            "BypassPermissions"
        ))));
        assert!(is_parseable_mode(Some(&serde_json::json!("  plan  "))));
        assert!(!is_parseable_mode(Some(&serde_json::json!("auto"))));
        assert!(!is_parseable_mode(Some(&serde_json::json!(42))));
    }

    #[test]
    fn a_valid_effective_mode_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        let home = dir.path().join("home");
        write(
            &home.join(".claude").join("settings.json"),
            r#"{"permissions":{"defaultMode":"default"}}"#,
        );
        ensure_parseable_permission_mode(&cwd, &home).unwrap();
        assert!(!cwd.join(".claude").join("settings.local.json").exists());
    }

    #[test]
    fn an_absent_effective_mode_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        let home = dir.path().join("home");
        ensure_parseable_permission_mode(&cwd, &home).unwrap();
        assert!(!cwd.join(".claude").join("settings.local.json").exists());
    }

    #[test]
    fn an_unparseable_user_mode_gets_a_local_baseline_written() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        let home = dir.path().join("home");
        write(
            &home.join(".claude").join("settings.json"),
            r#"{"permissions":{"defaultMode":"auto"}}"#,
        );
        ensure_parseable_permission_mode(&cwd, &home).unwrap();
        let local_file = cwd.join(".claude").join("settings.local.json");
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(local_file).unwrap()).unwrap();
        assert_eq!(written["permissions"]["defaultMode"], "default");
    }

    #[test]
    fn writing_the_baseline_preserves_every_other_key_the_user_owns() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        let home = dir.path().join("home");
        write(
            &home.join(".claude").join("settings.json"),
            r#"{"permissions":{"defaultMode":"auto"}}"#,
        );
        write(
            &cwd.join(".claude").join("settings.local.json"),
            r#"{"hooks":{"pre-commit":"lint"},"permissions":{"allow":["Bash(ls)"]}}"#,
        );
        ensure_parseable_permission_mode(&cwd, &home).unwrap();
        let local_file = cwd.join(".claude").join("settings.local.json");
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(local_file).unwrap()).unwrap();
        assert_eq!(written["permissions"]["defaultMode"], "default");
        assert_eq!(written["permissions"]["allow"][0], "Bash(ls)");
        assert_eq!(written["hooks"]["pre-commit"], "lint");
    }

    #[test]
    fn a_valid_local_mode_wins_over_an_invalid_project_mode() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        let home = dir.path().join("home");
        write(
            &cwd.join(".claude").join("settings.json"),
            r#"{"permissions":{"defaultMode":"auto"}}"#,
        );
        write(
            &cwd.join(".claude").join("settings.local.json"),
            r#"{"permissions":{"defaultMode":"plan"}}"#,
        );
        ensure_parseable_permission_mode(&cwd, &home).unwrap();
        // Unchanged — local (highest precedence) is already valid.
        let local_file = cwd.join(".claude").join("settings.local.json");
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(local_file).unwrap()).unwrap();
        assert_eq!(written["permissions"]["defaultMode"], "plan");
    }
}
