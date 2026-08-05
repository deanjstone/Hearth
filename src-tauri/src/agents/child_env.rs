// Builds the environment for a spawned ACP adapter subprocess.
//
// Normally the adapter inherits Hearth's full process env, with a little extra
// merged over it. But when Hearth runs *inside another agent* (e.g. a Claude Code
// dev shell), that parent leaks its own ANTHROPIC_API_KEY / ANTHROPIC_BASE_URL
// into our env — and the spawned agent picks them up and talks to the wrong
// gateway. HEARTH_SCRUB_INHERITED_KEYS=1 strips those inherited credential/gateway
// vars so the spawned agent uses ONLY the credential Hearth chose.
//
// Scrubbing happens on the INHERITED env only; `extra` (which carries the user's
// own BYO key) is merged AFTER the scrub, so it always wins. Ported from
// electron/main/agents/child-env.ts — shared with Phase 4's terminal port, same
// as the TS original is shared between agents/ and terminal/pty.ts.

use std::collections::HashMap;

/// Inherited env vars that, if leaked from a parent agent, hijack the spawned
/// adapter's credential or gateway. Scrubbed only when the flag is set.
pub const INHERITED_CREDENTIAL_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildChildEnvOptions {
    /// When true, drop INHERITED_CREDENTIAL_VARS from the base env before merging.
    pub scrub_inherited_keys: bool,
}

/// Compose the child env: base (usually the process env), optionally scrubbed,
/// with `extra` merged over the top. Pure so the scrub logic unit-tests without
/// a spawn. Never mutates `base`.
pub fn build_child_env(
    base: &HashMap<String, String>,
    extra: &HashMap<String, String>,
    opts: BuildChildEnvOptions,
) -> HashMap<String, String> {
    let mut out = base.clone();
    if opts.scrub_inherited_keys {
        for key in INHERITED_CREDENTIAL_VARS {
            out.remove(*key);
        }
    }
    for (k, v) in extra {
        out.insert(k.clone(), v.clone());
    }
    out
}

/// Read the opt-in flag from an env map.
pub fn should_scrub_inherited_keys(env: &HashMap<String, String>) -> bool {
    env.get("HEARTH_SCRUB_INHERITED_KEYS")
        .map(|v| v == "1")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn passes_base_env_through_and_merges_extra_over_it_no_scrub() {
        let base = map(&[("PATH", "/usr/bin"), ("FOO", "bar")]);
        let extra = map(&[("ELECTRON_RUN_AS_NODE", "1")]);
        let env = build_child_env(&base, &extra, BuildChildEnvOptions::default());
        assert_eq!(env.get("PATH").unwrap(), "/usr/bin");
        assert_eq!(env.get("FOO").unwrap(), "bar");
        assert_eq!(env.get("ELECTRON_RUN_AS_NODE").unwrap(), "1");
    }

    #[test]
    fn without_scrub_inherited_credential_vars_survive() {
        let base = map(&[
            ("ANTHROPIC_API_KEY", "leaked"),
            ("ANTHROPIC_BASE_URL", "http://gw"),
        ]);
        let env = build_child_env(&base, &HashMap::new(), BuildChildEnvOptions::default());
        assert_eq!(env.get("ANTHROPIC_API_KEY").unwrap(), "leaked");
        assert_eq!(env.get("ANTHROPIC_BASE_URL").unwrap(), "http://gw");
    }

    #[test]
    fn scrub_removes_every_inherited_credential_var_from_the_base() {
        let mut base = map(&[("PATH", "/usr/bin")]);
        for k in INHERITED_CREDENTIAL_VARS {
            base.insert(k.to_string(), "inherited".to_string());
        }
        let env = build_child_env(
            &base,
            &HashMap::new(),
            BuildChildEnvOptions {
                scrub_inherited_keys: true,
            },
        );
        for k in INHERITED_CREDENTIAL_VARS {
            assert!(env.get(*k).is_none(), "{k} should have been scrubbed");
        }
        assert_eq!(env.get("PATH").unwrap(), "/usr/bin"); // unrelated vars untouched
    }

    #[test]
    fn a_byo_key_in_extra_wins_over_a_scrubbed_inherited_key() {
        let base = map(&[("ANTHROPIC_API_KEY", "leaked-from-parent")]);
        let extra = map(&[("ANTHROPIC_API_KEY", "users-own-key")]);
        let env = build_child_env(
            &base,
            &extra,
            BuildChildEnvOptions {
                scrub_inherited_keys: true,
            },
        );
        assert_eq!(env.get("ANTHROPIC_API_KEY").unwrap(), "users-own-key");
    }

    #[test]
    fn scrub_with_no_extra_leaves_the_credential_unset() {
        let base = map(&[("ANTHROPIC_API_KEY", "leaked")]);
        let env = build_child_env(
            &base,
            &HashMap::new(),
            BuildChildEnvOptions {
                scrub_inherited_keys: true,
            },
        );
        assert!(env.get("ANTHROPIC_API_KEY").is_none());
    }

    #[test]
    fn does_not_mutate_the_base_env_map() {
        let base = map(&[("ANTHROPIC_API_KEY", "leaked")]);
        let _ = build_child_env(
            &base,
            &HashMap::new(),
            BuildChildEnvOptions {
                scrub_inherited_keys: true,
            },
        );
        assert_eq!(base.get("ANTHROPIC_API_KEY").unwrap(), "leaked");
    }

    #[test]
    fn should_scrub_true_only_when_flag_is_exactly_one() {
        assert!(should_scrub_inherited_keys(&map(&[(
            "HEARTH_SCRUB_INHERITED_KEYS",
            "1"
        )])));
        assert!(!should_scrub_inherited_keys(&map(&[(
            "HEARTH_SCRUB_INHERITED_KEYS",
            "true"
        )])));
        assert!(!should_scrub_inherited_keys(&HashMap::new()));
    }
}
