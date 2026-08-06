// Ported from electron/main/micro-apps/validate.ts (Phase 6, tracking issue
// #27). A name becomes a path segment under micro-apps/<name> and a
// capability-store key, so every entry point that takes a renderer-supplied
// name must validate it — not just scaffold (create). Without this, names
// like "../../x" escape the micro-apps dir (arbitrary dir spawn / file read)
// or poison the capability store.

/// `^[a-z0-9][a-z0-9_-]{0,63}$`, hand-checked rather than pulling in the
/// `regex` crate for one anchored character-class pattern (`fancy-regex` is
/// already a dependency for shell_guard.rs, but that's for backtracking
/// features this pattern doesn't need).
fn is_valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Reject a name that isn't a safe micro-app identifier. Returns the trimmed
/// name.
pub fn assert_app_name(raw_name: &str) -> Result<String, String> {
    let name = raw_name.trim().to_string();
    if !is_valid_name(&name) {
        return Err(
            "Invalid app name. Use lowercase letters, numbers, \"-\" and \"_\".".to_string(),
        );
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_plain_lowercase_name() {
        assert_eq!(assert_app_name("my-app").unwrap(), "my-app");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(assert_app_name("  my-app  ").unwrap(), "my-app");
    }

    #[test]
    fn accepts_digits_and_underscores() {
        assert_eq!(assert_app_name("app_2").unwrap(), "app_2");
    }

    #[test]
    fn rejects_uppercase() {
        assert!(assert_app_name("MyApp").is_err());
    }

    #[test]
    fn rejects_a_leading_hyphen() {
        assert!(assert_app_name("-app").is_err());
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(assert_app_name("../../etc").is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(assert_app_name("").is_err());
        assert!(assert_app_name("   ").is_err());
    }

    #[test]
    fn rejects_over_64_chars() {
        let long = "a".repeat(65);
        assert!(assert_app_name(&long).is_err());
    }

    #[test]
    fn accepts_exactly_64_chars() {
        let name = "a".repeat(64);
        assert!(assert_app_name(&name).is_ok());
    }
}
