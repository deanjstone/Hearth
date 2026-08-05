// Presence check for a backend's OWN stored login, used to show truthful auth
// status for the INACTIVE backend (the one without a spawned adapter) so the user
// can see both backends are usable and switch freely. Ported from
// electron/main/agents/login-presence.ts.
//
// COMPLIANCE.md: presence/expiry ONLY — this never reads, stores, brokers, or logs
// a token value. Codex's auth.json is checked by existence; Claude's `oauthAccount`
// is non-secret account metadata (the OAuth token lives in the OS keychain,
// untouched).

use super::agent::AgentKind;
use std::fs;
use std::path::Path;

/// Presence-only login check, parameterized by `home` so it's testable against a
/// fixture directory instead of the real one.
pub fn has_stored_login(kind: AgentKind, home: &Path) -> bool {
    match kind {
        AgentKind::Codex => home.join(".codex").join("auth.json").exists(),
        AgentKind::Claude => {
            if home.join(".claude").join(".credentials.json").exists() {
                return true;
            }
            let claude_json = home.join(".claude.json");
            let Ok(contents) = fs::read_to_string(&claude_json) else {
                return false;
            };
            let Ok(root) = serde_json::from_str::<serde_json::Value>(&contents) else {
                return false;
            };
            root.get("oauthAccount").is_some_and(|v| !v.is_null())
        }
    }
}

/// `has_stored_login` against the real home directory.
pub fn has_stored_login_default(kind: AgentKind) -> bool {
    match dirs::home_dir() {
        Some(home) => has_stored_login(kind, &home),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};

    fn temp_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn codex_present_only_when_auth_json_exists() {
        let home = temp_home();
        assert!(!has_stored_login(AgentKind::Codex, home.path()));
        create_dir_all(home.path().join(".codex")).unwrap();
        write(home.path().join(".codex").join("auth.json"), "{}").unwrap();
        assert!(has_stored_login(AgentKind::Codex, home.path()));
    }

    #[test]
    fn claude_present_via_credentials_file() {
        let home = temp_home();
        create_dir_all(home.path().join(".claude")).unwrap();
        write(home.path().join(".claude").join(".credentials.json"), "{}").unwrap();
        assert!(has_stored_login(AgentKind::Claude, home.path()));
    }

    #[test]
    fn claude_present_via_oauth_account_marker_in_claude_json() {
        let home = temp_home();
        write(
            home.path().join(".claude.json"),
            r#"{"oauthAccount":{"email":"x@example.com"}}"#,
        )
        .unwrap();
        assert!(has_stored_login(AgentKind::Claude, home.path()));
    }

    #[test]
    fn claude_absent_when_neither_file_present() {
        let home = temp_home();
        assert!(!has_stored_login(AgentKind::Claude, home.path()));
    }

    #[test]
    fn claude_absent_when_claude_json_has_no_oauth_account() {
        let home = temp_home();
        write(home.path().join(".claude.json"), r#"{"other":true}"#).unwrap();
        assert!(!has_stored_login(AgentKind::Claude, home.path()));
    }

    #[test]
    fn claude_absent_when_oauth_account_is_null() {
        let home = temp_home();
        write(home.path().join(".claude.json"), r#"{"oauthAccount":null}"#).unwrap();
        assert!(!has_stored_login(AgentKind::Claude, home.path()));
    }

    #[test]
    fn corrupt_claude_json_treated_as_absent_not_a_crash() {
        let home = temp_home();
        write(home.path().join(".claude.json"), "{not json").unwrap();
        assert!(!has_stored_login(AgentKind::Claude, home.path()));
    }
}
