// Startup Node/adapter-binary availability check (Chunk 4, spec #48; decision
// from grilling ticket #47). Runs once during `setup()` (wired in lib.rs),
// the same phase `BootWatchdog::inspect_boot()` runs in today, and its result
// is cached on `AppState` (see `agents_commands.rs`). Blast radius is
// agent-chat only — self-mod, the terminal, and every other subsystem are
// unaffected by a failed check, matching spec #26's own git-availability
// precedent ("a missing git only degrades the self-mod History view... it
// doesn't block the rest of the app").
//
// Checks three things eagerly rather than lazily inside `connect()`, so a
// missing/corrupt vendored adapter package surfaces as one clear failure at
// boot instead of a confusing spawn error deep inside a later agent-chat
// attempt: `node` on PATH, then both `claude::PACKAGE` and `codex::PACKAGE`
// resolving via the exact same `resolve_adapter_bin` algorithm `claude.rs`/
// `codex.rs` use for the real spawn — no separate resolution logic
// duplicated here, so this can't silently drift from what actually runs.

use super::acp_client::resolve_adapter_bin;
use super::{claude, codex};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum AgentRuntimeStatus {
    Ok,
    NodeMissing,
    AdapterMissing { package: String },
}

/// `path_var` is injected (like `home` in `login_presence.rs`/`claude.rs`),
/// so this is testable against a fixture PATH instead of the process's real
/// one — same style as `boot_watchdog.rs`'s tempdir-fixture tests.
pub fn check(repo_root: &Path, path_var: &str) -> AgentRuntimeStatus {
    if !node_on_path(path_var) {
        return AgentRuntimeStatus::NodeMissing;
    }
    for (package, bin) in [(claude::PACKAGE, claude::BIN), (codex::PACKAGE, codex::BIN)] {
        if resolve_adapter_bin(repo_root, package, bin).is_err() {
            return AgentRuntimeStatus::AdapterMissing {
                package: package.to_string(),
            };
        }
    }
    AgentRuntimeStatus::Ok
}

fn node_on_path(path_var: &str) -> bool {
    std::env::split_paths(path_var).any(|dir| is_executable_file(&dir.join("node")))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn make_executable_node(bin_dir: &Path) {
        fs::create_dir_all(bin_dir).unwrap();
        let node = bin_dir.join("node");
        fs::write(&node, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write_pkg(node_modules: &Path, package: &str, bin_name: &str) {
        let dir = node_modules.join(package);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            format!(r#"{{"name":"{package}","bin":{{"{bin_name}":"dist/index.js"}}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn no_node_on_path_reports_node_missing() {
        let dir = tempfile::tempdir().unwrap();
        let empty_bin = dir.path().join("empty-bin");
        fs::create_dir_all(&empty_bin).unwrap();
        assert_eq!(
            check(dir.path(), &empty_bin.to_string_lossy()),
            AgentRuntimeStatus::NodeMissing
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_executable_node_file_does_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::write(bin_dir.join("node"), "not executable").unwrap();
        assert_eq!(
            check(dir.path(), &bin_dir.to_string_lossy()),
            AgentRuntimeStatus::NodeMissing
        );
    }

    #[test]
    fn node_present_but_no_adapters_reports_claude_missing_first() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        make_executable_node(&bin_dir);
        assert_eq!(
            check(dir.path(), &bin_dir.to_string_lossy()),
            AgentRuntimeStatus::AdapterMissing {
                package: claude::PACKAGE.to_string()
            }
        );
    }

    #[test]
    fn claude_present_codex_missing_reports_codex_missing() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        make_executable_node(&bin_dir);
        write_pkg(
            &dir.path().join("node_modules"),
            claude::PACKAGE,
            claude::BIN,
        );
        assert_eq!(
            check(dir.path(), &bin_dir.to_string_lossy()),
            AgentRuntimeStatus::AdapterMissing {
                package: codex::PACKAGE.to_string()
            }
        );
    }

    #[test]
    fn node_and_both_adapters_present_reports_ok() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        make_executable_node(&bin_dir);
        let node_modules = dir.path().join("node_modules");
        write_pkg(&node_modules, claude::PACKAGE, claude::BIN);
        write_pkg(&node_modules, codex::PACKAGE, codex::BIN);
        assert_eq!(
            check(dir.path(), &bin_dir.to_string_lossy()),
            AgentRuntimeStatus::Ok
        );
    }
}
