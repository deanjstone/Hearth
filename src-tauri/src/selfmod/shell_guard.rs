// Source-write enforcement (W0b). Ported from electron/main/self-mod/shell-guard.ts
// unchanged — detects shell commands that mutate repo source outside the agent's
// Edit/Write tool, so agent_commands.rs's permission-request handling (Chunk 5,
// spec #48, user story 21) can auto-reject them (Codex + as a universal backstop)
// instead of prompting the user. Heuristic tripwire, NOT the enforcement boundary:
// the commit-time scope guard (selfmod::service) is the real net, and the fs-watch
// overlay is the final backstop for anything that slips through.
//
// Scoped to *hand-edited source* mutation — deliberately does NOT block reads,
// builds, package installs, or writes to generated/output dirs. Interpreter-based
// writes (node -e "fs.writeFileSync(...)", python -c "open(...,'w')") are also out
// of scope here by design — they're too varied to match without false positives
// and are caught by the commit-time guard.
//
// Uses `fancy-regex`, not the standard `regex` crate — see Cargo.toml's comment
// on the dependency for why (REDIRECT's negative lookahead has no non-backtracking
// equivalent).

use fancy_regex::Regex;
use std::sync::LazyLock;

// Commands that write files in place.
static MUTATORS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(sed\s+-i|tee\b|dd\b|truncate\b|install\s+-|patch\b)").unwrap()
});
// Redirections into a file: `>path`, `> path`, `>>path`, `1> path` (an optional fd
// digit). Excludes fd duplication (`2>&1`, `&>`, `>&`) and process substitution
// (`>(...)`).
static REDIRECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^&\d>])\d?>>?\s*(?![&(])\S").unwrap());
// File-moving commands.
static MOVERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(cp|mv|rsync|ln)\s+").unwrap());

// Paths we protect from un-mediated shell writes (repo source, not build output).
// The boundary class includes `>` so a no-space redirect (`>>src/x`) still counts.
static SOURCE_HINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^|[\s'">/])(?:src/|electron/|index\.html|package\.json|electron\.vite\.config|tsconfig)"#)
        .unwrap()
});

/// True when `command` looks like it mutates repo source without going through
/// the mediated Edit/Write tool. Heuristic by design — the fs-watch backstop
/// catches the rest. Returns false for reads, builds, installs, and
/// generated-output writes.
pub fn is_source_mutating_shell(command: &str) -> bool {
    let c = command.trim();
    if c.is_empty() {
        return false;
    }
    // A write only counts if it's an in-place mutator, a file redirect, or a
    // move — reads (cat/grep/ls), builds, installs, and tests have none of
    // these, so they fall through to false even when they mention `src/`.
    let mutates = MUTATORS.is_match(c).unwrap_or(false)
        || MOVERS.is_match(c).unwrap_or(false)
        || REDIRECT.is_match(c).unwrap_or(false);
    if !mutates {
        return false;
    }
    SOURCE_HINT.is_match(c).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_source_mutating_shell_commands() {
        let blocked = [
            "sed -i '' 's/a/b/' src/shell/Rail.tsx",
            "echo \"x\" > src/app/Chat.tsx",
            "cat foo >> electron/main/index.ts",
            "tee src/styles/hearth.css",
            "cp /tmp/x.tsx src/app/x.tsx",
            "mv old.tsx src/app/new.tsx",
            "echo x >src/app/foo.tsx",               // no space after >
            "echo x>>src/app/foo.tsx",               // no spaces at all
            "printf data 1> electron/main/index.ts", // fd-qualified redirect to a file
        ];
        for c in blocked {
            assert!(is_source_mutating_shell(c), "expected to block: {c}");
        }
    }

    #[test]
    fn allows_reads_builds_and_non_source_writes() {
        let allowed = [
            "cat src/app/Chat.tsx", // read
            "grep -r foo src/",     // read
            "bun run typecheck",
            "bun test src/x.test.ts",
            "git status",
            "git diff src/app/Chat.tsx",
            "eslint src/",
            "echo \"log\" > /tmp/out.log", // not source
            "echo hi >> dist/bundle.js",   // generated output, not source
            "ls src/",
            "tsc -p tsconfig.json 2>&1", // 2>&1 is fd-dup, not a write — even with a source mention
            "cat src/x.ts > (process subst is not a file write)", // > ( is process substitution
        ];
        for c in allowed {
            assert!(!is_source_mutating_shell(c), "expected to allow: {c}");
        }
    }

    #[test]
    fn empty_command_is_never_mutating() {
        assert!(!is_source_mutating_shell(""));
        assert!(!is_source_mutating_shell("   "));
    }
}
