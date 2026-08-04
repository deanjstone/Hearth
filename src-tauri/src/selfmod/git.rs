// Git-backed history for self-modifications, via the system `git` binary. Every
// agent edit lands as a commit; "undo" is a revert. This is the safety net that
// makes letting an agent rewrite the app survivable.
//
// `Command::new("git")` is spawned with an explicit argv array and no shell, so
// there is no command-injection surface even though args are dynamic — mirrors
// dugite's `exec(args, cwd)` in the Electron original
// (electron/main/self-mod/git.ts).

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug)]
pub struct GitError(String);

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for GitError {}

struct RunResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn run_git(repo_root: &Path, args: &[&str]) -> RunResult {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .expect("failed to spawn git");
    RunResult {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn git(repo_root: &Path, args: &[&str]) -> Result<String, GitError> {
    let r = run_git(repo_root, args);
    if r.exit_code != 0 {
        return Err(GitError(format!(
            "git {} failed ({}): {}",
            args.join(" "),
            r.exit_code,
            r.stderr.trim()
        )));
    }
    Ok(r.stdout)
}

/// Initialize a fresh repo at `repo_root` and commit everything in it as the
/// baseline. Used to seed the packaged app's writable source workspace. Sets a
/// local commit identity so the commit succeeds even when global git config is
/// empty (a clean machine with no `git config user.*`).
pub fn init_baseline_repo(repo_root: &Path, subject: &str) -> Result<String, GitError> {
    git(repo_root, &["init"])?;
    git(repo_root, &["config", "user.email", "hearth@localhost"])?;
    git(repo_root, &["config", "user.name", "Hearth"])?;
    git(repo_root, &["add", "-A"])?;
    git(repo_root, &["commit", "-m", subject])?;
    Ok(git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string())
}

/// Stage and commit every change, but only if the tree is dirty (a no-op commit
/// fails). Returns the new HEAD, or `None` when there was nothing to commit.
pub fn commit_all(repo_root: &Path, subject: &str) -> Result<Option<String>, GitError> {
    if list_dirty(repo_root)?.is_empty() {
        return Ok(None);
    }
    git(repo_root, &["add", "-A"])?;
    git(repo_root, &["commit", "-m", subject])?;
    Ok(Some(
        git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string(),
    ))
}

pub fn list_dirty(repo_root: &Path) -> Result<Vec<String>, GitError> {
    // -z gives NUL-separated, *unquoted* paths. Without it, porcelain C-quotes any
    // path containing spaces or special chars, which would leak literal quotes
    // into the returned path.
    // --untracked-files=all lists every untracked FILE individually; without it
    // git collapses a wholly-new directory into a single `?? dir/` entry, which
    // would break per-path self-mod grouping when a subagent creates a new folder.
    let out = git(
        repo_root,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )?;
    let records: Vec<&str> = out.split('\0').filter(|s| !s.is_empty()).collect();
    // Each record is `XY <path>`. A rename/copy (R/C) is followed by an extra
    // `<oldpath>` record, which we skip — we want the current path only.
    let mut paths = Vec::new();
    let mut i = 0;
    while i < records.len() {
        let rec = records[i];
        let status = &rec[0..2];
        paths.push(rec[3..].to_string());
        if status.as_bytes()[0] == b'R' || status.as_bytes()[0] == b'C' {
            i += 1; // consume the old-path field
        }
        i += 1;
    }
    Ok(paths)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

/// Rename/copy pairs in the working tree. `list_dirty`/`list_tracked_dirty`
/// deliberately drop the old path (callers want current paths), but the
/// commit-time scope guard needs both sides: a `git mv <protected> <canvas>`
/// would otherwise slip the island deletion past the canvas filter.
pub fn list_renames(repo_root: &Path) -> Result<Vec<Rename>, GitError> {
    let out = git(
        repo_root,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )?;
    let records: Vec<&str> = out.split('\0').filter(|s| !s.is_empty()).collect();
    let mut renames = Vec::new();
    let mut i = 0;
    while i < records.len() {
        let status = &records[i][0..2];
        let to = records[i][3..].to_string();
        if status.as_bytes()[0] == b'R' || status.as_bytes()[0] == b'C' {
            i += 1; // the record after an R/C is its old path
            if let Some(from) = records.get(i).filter(|s| !s.is_empty()) {
                renames.push(Rename {
                    from: from.to_string(),
                    to,
                });
            }
        }
        i += 1;
    }
    Ok(renames)
}

/// Tracked working-tree changes only (staged or unstaged) — untracked files are
/// excluded. This is what a `git revert` can actually conflict with: a revert
/// never touches untracked files, so the undo/redo guard must check THIS, not
/// `list_dirty`. (`list_dirty` includes untracked because the commit flow needs
/// to capture new files an agent creates; the guard has the opposite requirement.)
pub fn list_tracked_dirty(repo_root: &Path) -> Result<Vec<String>, GitError> {
    let out = git(repo_root, &["status", "--porcelain", "-z"])?;
    let records: Vec<&str> = out.split('\0').filter(|s| !s.is_empty()).collect();
    let mut paths = Vec::new();
    let mut i = 0;
    while i < records.len() {
        let rec = records[i];
        let status = &rec[0..2];
        if status != "??" {
            paths.push(rec[3..].to_string());
        }
        if status.as_bytes()[0] == b'R' || status.as_bytes()[0] == b'C' {
            i += 1; // consume the old-path field
        }
        i += 1;
    }
    Ok(paths)
}

/// Which surface a self-mod belongs to (derived from the files it changed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfModKind {
    Code,
    Soul,
    Memory,
}

impl SelfModKind {
    fn as_str(self) -> &'static str {
        match self {
            SelfModKind::Code => "code",
            SelfModKind::Soul => "soul",
            SelfModKind::Memory => "memory",
        }
    }
}

// Repo-tracked source of truth for the global soul/memory (see SOUL-AND-MEMORY.md).
const SOUL_FILES: &[&str] = &[".hearth/personality.json"];
const MEMORY_FILES: &[&str] = &[".hearth/memory.md"];

/// Categorize a self-mod by its changed paths: pure soul/memory edits route to
/// those surfaces; anything else (or a mix) is code.
pub fn categorize_kind(paths: &[String]) -> SelfModKind {
    if paths.is_empty() {
        return SelfModKind::Code;
    }
    let all = |set: &[&str]| paths.iter().all(|p| set.contains(&p.as_str()));
    if all(SOUL_FILES) {
        SelfModKind::Soul
    } else if all(MEMORY_FILES) {
        SelfModKind::Memory
    } else {
        SelfModKind::Code
    }
}

pub struct SelfModCommit {
    /// Files to stage (repo-relative). `None`/empty = stage everything dirty.
    pub paths: Option<Vec<String>>,
    pub subject: String,
    /// Conversation that produced the change; recorded as a trailer for revert routing.
    pub conversation_id: String,
    /// Surface category; `None` derives from `paths` via `categorize_kind`.
    pub kind: Option<SelfModKind>,
    /// Run id grouping the per-subagent commits of one turn.
    pub run_id: Option<String>,
    /// Subagent label this commit's files were attributed to.
    pub subagent: Option<String>,
    /// Marks a commit that recovered an incomplete (crashed mid-turn) run.
    pub recovered: bool,
}

pub fn commit_self_mod(repo_root: &Path, c: &SelfModCommit) -> Result<String, GitError> {
    let kind = c
        .kind
        .unwrap_or_else(|| categorize_kind(c.paths.as_deref().unwrap_or(&[])));
    let mut trailers = vec![
        format!("Hearth-Conversation: {}", c.conversation_id),
        format!("Hearth-Kind: {}", kind.as_str()),
    ];
    if let Some(run_id) = &c.run_id {
        trailers.push(format!("Hearth-Run: {run_id}"));
    }
    if let Some(subagent) = &c.subagent {
        trailers.push(format!("Hearth-Subagent: {subagent}"));
    }
    if c.recovered {
        trailers.push("Hearth-Recovered: true".to_string());
    }
    trailers.push("Hearth-SelfMod: true".to_string());
    let message = format!("{}\n\n{}", c.subject, trailers.join("\n"));

    match c.paths.as_deref() {
        Some(paths) if !paths.is_empty() => {
            // Stage AND commit only these paths (pathspec on commit) so any other
            // dirty or already-staged files in the tree are left untouched — the
            // self-mod commit must contain ONLY what this turn changed, never the
            // developer's unrelated work.
            let mut add_args: Vec<&str> = vec!["add", "--"];
            add_args.extend(paths.iter().map(String::as_str));
            git(repo_root, &add_args)?;

            let mut commit_args: Vec<&str> = vec!["commit", "-m", message.as_str(), "--"];
            commit_args.extend(paths.iter().map(String::as_str));
            git(repo_root, &commit_args)?;
        }
        _ => {
            git(repo_root, &["add", "-A"])?;
            git(repo_root, &["commit", "-m", message.as_str()])?;
        }
    }
    Ok(git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string())
}

/// Undo the working-tree changes to specific paths (commit-time enforcement):
/// restore a tracked path from HEAD, or delete a path that's new (absent from
/// HEAD). Used to reject an agent write to a blocked/protected path so it never
/// commits.
pub fn restore_paths(repo_root: &Path, paths: &[String]) {
    for p in paths {
        let r = run_git(repo_root, &["checkout", "HEAD", "--", p]);
        if r.exit_code != 0 {
            // Not in HEAD → it's a newly-created file; remove it from the working tree.
            let _ = fs::remove_file(repo_root.join(p));
        }
    }
}

/// Revert a specific self-mod commit (does not touch later unrelated commits).
///
/// Called by the boot watchdog to undo a self-mod that bricked startup. A prior
/// turn may have left the tree dirty — uncommitted edits, or untracked files
/// that would block `git revert`. At a bricked boot those changes are either the
/// bad edit itself or orphaned, so we discard them first to guarantee the revert
/// can apply. reset/clean leave gitignored paths (node_modules, .hearth) untouched.
pub fn revert_commit(repo_root: &Path, hash: &str) -> Result<String, GitError> {
    git(repo_root, &["reset", "--hard", "HEAD"])?;
    git(repo_root, &["clean", "-fd"])?;
    git(repo_root, &["revert", "--no-edit", hash])?;
    Ok(git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevertOutcome {
    Reverted { commit: String },
    Conflict { files: Vec<String> },
}

/// Revert `hash`, but on a merge conflict capture the unmerged files and ABORT
/// (leaving a clean tree) rather than leaving Hearth mid-revert — the caller
/// hands the conflict to the agent to resolve from clean state. Non-conflict
/// failures return `Err`.
pub fn try_revert(repo_root: &Path, hash: &str) -> Result<RevertOutcome, GitError> {
    let r = run_git(repo_root, &["revert", "--no-edit", hash]);
    if r.exit_code == 0 {
        let commit = git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string();
        return Ok(RevertOutcome::Reverted { commit });
    }
    let conflicted: Vec<String> =
        git(repo_root, &["diff", "--name-only", "--diff-filter=U", "-z"])?
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
    let _ = run_git(repo_root, &["revert", "--abort"]); // restore a clean tree either way
    if !conflicted.is_empty() {
        return Ok(RevertOutcome::Conflict { files: conflicted });
    }
    Err(GitError(format!(
        "git revert {} failed ({}): {}",
        hash,
        r.exit_code,
        r.stderr.trim()
    )))
}

/// Repo-relative paths a given commit changed. Used to pick the HMR reload tier
/// after a revert.
pub fn diff_paths(repo_root: &Path, hash: &str) -> Result<Vec<String>, GitError> {
    // --root so a root (parentless) commit still reports its files.
    let out = git(
        repo_root,
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-only",
            "-r",
            hash,
        ],
    )?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfModLogEntry {
    pub hash: String,
    pub subject: String,
    pub conversation_id: Option<String>,
    pub kind: SelfModKind,
    /// Run id this commit belongs to, or `None` for ungrouped/legacy.
    pub run_id: Option<String>,
    /// Subagent label, or `None` for orchestrator/legacy commits.
    pub subagent: Option<String>,
    /// True if a later `git revert` commit already undid this one.
    pub reverted: bool,
}

// ── Net-effect undo/redo (Model A) — pure helpers ──────────────────────────
// `git revert X` writes "This reverts commit <40-hex>." So we build a graph of
// "which commit reverts which" and compute each self-mod's *logical* state:
// applied iff the number of its *effective* reverts (reverts that are
// themselves still applied) is even. Redo = revert-the-revert, so the chain
// must be followed, not just "does any revert exist". These are pure so they
// unit-test without git.

#[derive(Debug, Clone)]
pub struct RawCommit {
    pub hash: String,
    /// Commit body (where `git revert` records "This reverts commit <hash>.").
    pub body: String,
}

fn is_lower_hex(c: char) -> bool {
    c.is_ascii_digit() || ('a'..='f').contains(&c)
}

/// The full target hash a revert commit points at, or `None` if it isn't a revert.
pub fn parse_revert_target(body: &str) -> Option<String> {
    const MARKER: &str = "This reverts commit ";
    let idx = body.find(MARKER)?;
    let rest = &body[idx + MARKER.len()..];
    let hex: String = rest
        .chars()
        .take_while(|&c| is_lower_hex(c))
        .take(40)
        .collect();
    if hex.len() >= 7 {
        Some(hex)
    } else {
        None
    }
}

/// Map: target hash → revert-commit hashes that revert it (newest first, as logged).
pub fn build_revert_graph(commits: &[RawCommit]) -> HashMap<String, Vec<String>> {
    let mut reverts: HashMap<String, Vec<String>> = HashMap::new();
    for c in commits {
        if let Some(target) = parse_revert_target(&c.body) {
            reverts.entry(target).or_default().push(c.hash.clone());
        }
    }
    reverts
}

/// Logical state: a commit is applied iff its effective (themselves-applied)
/// reverts net out even. Recurses through revert-of-revert chains.
pub fn is_applied(
    hash: &str,
    reverts: &HashMap<String, Vec<String>>,
    memo: &mut HashMap<String, bool>,
) -> bool {
    if let Some(&cached) = memo.get(hash) {
        return cached;
    }
    memo.insert(hash.to_string(), true); // guard cycles (shouldn't happen in a commit DAG)
    let effective = reverts
        .get(hash)
        .map(|list| list.iter().filter(|r| is_applied(r, reverts, memo)).count())
        .unwrap_or(0);
    let applied = effective.is_multiple_of(2);
    memo.insert(hash.to_string(), applied);
    applied
}

/// The revert to undo when redoing `hash`: its newest effective (applied) revert.
pub fn effective_revert_of(hash: &str, reverts: &HashMap<String, Vec<String>>) -> Option<String> {
    let mut memo = HashMap::new();
    reverts
        .get(hash)?
        .iter()
        .find(|r| is_applied(r, reverts, &mut memo))
        .cloned()
}

fn all_commits(repo_root: &Path, limit: usize) -> Result<Vec<RawCommit>, GitError> {
    let n_arg = format!("-n{limit}");
    let out = git(repo_root, &["log", &n_arg, "-z", "--pretty=%H%x1f%b"])?;
    Ok(out
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|rec| match rec.find('\u{1f}') {
            Some(idx) => RawCommit {
                hash: rec[..idx].to_string(),
                body: rec[idx + 1..].to_string(),
            },
            None => RawCommit {
                hash: rec.to_string(),
                body: String::new(),
            },
        })
        .collect())
}

/// Recent self-mod commits, newest first, with net-effect applied/undone state.
/// `limit` mirrors the TS default of 50 (Rust has no default-argument sugar).
pub fn recent_self_mods(repo_root: &Path, limit: usize) -> Result<Vec<SelfModLogEntry>, GitError> {
    let n_arg = format!("-n{limit}");
    let out = git(
        repo_root,
        &[
            "log",
            &n_arg,
            // NUL-separate commits so trailer values (which carry newlines) don't
            // split records.
            "-z",
            "--grep=Hearth-SelfMod: true",
            "--pretty=%H%x1f%s%x1f%(trailers:key=Hearth-Conversation,valueonly)%x1f%(trailers:key=Hearth-Kind,valueonly)%x1f%(trailers:key=Hearth-Run,valueonly)%x1f%(trailers:key=Hearth-Subagent,valueonly)",
        ],
    )?;

    // Look back further for reverts than for the self-mods themselves — an old
    // self-mod can be reverted by a very recent commit and vice versa.
    let reverts = build_revert_graph(&all_commits(repo_root, limit.saturating_mul(6).max(300))?);
    let mut memo = HashMap::new();

    Ok(out
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|line| {
            let fields: Vec<&str> = line.split('\u{1f}').collect();
            let hash = fields.first().copied().unwrap_or("").to_string();
            let subject = fields.get(1).copied().unwrap_or("").to_string();
            let conversation_id = fields
                .get(2)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let kind = match fields.get(3).map(|s| s.trim()).unwrap_or("") {
                "soul" => SelfModKind::Soul,
                "memory" => SelfModKind::Memory,
                _ => SelfModKind::Code,
            };
            let run_id = fields
                .get(4)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let subagent = fields
                .get(5)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let reverted = !is_applied(&hash, &reverts, &mut memo);
            SelfModLogEntry {
                hash,
                subject,
                conversation_id,
                kind,
                run_id,
                subagent,
                reverted,
            }
        })
        .collect())
}

/// The revert commit to revert in order to redo (re-apply) `hash`, or `None`.
pub fn redo_target(repo_root: &Path, hash: &str) -> Result<Option<String>, GitError> {
    let reverts = build_revert_graph(&all_commits(repo_root, 300)?);
    Ok(effective_revert_of(hash, &reverts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::{tempdir, TempDir};

    fn run(repo: &Path, args: &[&str]) -> String {
        git(repo, args).unwrap_or_else(|e| panic!("fixture git command failed: {e}"))
    }

    fn head(repo: &Path) -> String {
        run(repo, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn log_subjects(repo: &Path) -> Vec<String> {
        run(repo, &["log", "--pretty=%s"])
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn baseline_repo() -> TempDir {
        let dir = tempdir().expect("tempdir");
        let repo = dir.path();
        run(repo, &["init", "--initial-branch=main"]);
        run(repo, &["config", "user.name", "Test"]);
        run(repo, &["config", "user.email", "test@example.com"]);
        run(repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("baseline.txt"), "baseline\n").unwrap();
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-m", "baseline"]);
        dir
    }

    fn plain_commit(repo: &Path, subject: &str, conversation_id: &str) -> String {
        commit_self_mod(
            repo,
            &SelfModCommit {
                paths: None,
                subject: subject.to_string(),
                conversation_id: conversation_id.to_string(),
                kind: None,
                run_id: None,
                subagent: None,
                recovered: false,
            },
        )
        .unwrap()
    }

    #[test]
    fn list_tracked_dirty_ignores_untracked_but_reports_tracked_modifications() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        run(repo, &["init", "-b", "main"]);
        run(repo, &["config", "user.email", "t@h.dev"]);
        run(repo, &["config", "user.name", "T"]);
        fs::write(repo.join("tracked.txt"), "v1\n").unwrap();
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-m", "base"]);

        fs::write(repo.join("UNTRACKED.md"), "wip\n").unwrap();
        assert!(list_tracked_dirty(repo).unwrap().is_empty());
        assert!(list_dirty(repo)
            .unwrap()
            .contains(&"UNTRACKED.md".to_string()));

        fs::write(repo.join("tracked.txt"), "v2\n").unwrap();
        assert!(list_tracked_dirty(repo)
            .unwrap()
            .contains(&"tracked.txt".to_string()));
    }

    #[test]
    fn try_revert_conflict_reports_files_and_leaves_clean_tree() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        run(repo, &["init", "-b", "main"]);
        run(repo, &["config", "user.email", "t@h.dev"]);
        run(repo, &["config", "user.name", "T"]);
        fs::write(repo.join("f.txt"), "line1\nline2\nline3\n").unwrap();
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-m", "base"]);
        fs::write(repo.join("f.txt"), "line1\nX-change\nline3\n").unwrap();
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-m", "X"]);
        let x = head(repo);
        fs::write(repo.join("f.txt"), "line1\nlater-change\nline3\n").unwrap();
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-m", "later"]);

        let outcome = try_revert(repo, &x).unwrap();
        match outcome {
            RevertOutcome::Conflict { files } => assert!(files.contains(&"f.txt".to_string())),
            RevertOutcome::Reverted { .. } => panic!("expected a conflict"),
        }
        assert!(list_dirty(repo).unwrap().is_empty());
    }

    #[test]
    fn parse_revert_target_reads_the_reverted_hash() {
        assert_eq!(
            parse_revert_target("subject\n\nThis reverts commit abc1234."),
            Some("abc1234".to_string())
        );
        assert_eq!(parse_revert_target("an ordinary commit body"), None);
    }

    #[test]
    fn applied_state_follows_the_revert_of_revert_chain() {
        let x = "aaaaaaa";
        let r1 = "bbbbbbb";
        let r2 = "ccccccc";
        // newest first: R2 reverts R1, R1 reverts X
        let reverts = build_revert_graph(&[
            RawCommit {
                hash: r2.to_string(),
                body: format!("Revert revert\n\nThis reverts commit {r1}."),
            },
            RawCommit {
                hash: r1.to_string(),
                body: format!("Revert X\n\nThis reverts commit {x}."),
            },
            RawCommit {
                hash: x.to_string(),
                body: "edit X".to_string(),
            },
        ]);
        // X undone once (R1) then redone (R2 cancels R1) → X is applied again.
        let mut memo = HashMap::new();
        assert!(is_applied(x, &reverts, &mut memo));
        let mut memo = HashMap::new();
        assert!(!is_applied(r1, &reverts, &mut memo)); // R1 itself is reverted by R2
        assert_eq!(effective_revert_of(x, &reverts), None); // nothing to redo for X
    }

    #[test]
    fn single_revert_leaves_the_commit_undone_with_a_redo_target() {
        let x = "aaaaaaa";
        let r1 = "bbbbbbb";
        let reverts = build_revert_graph(&[
            RawCommit {
                hash: r1.to_string(),
                body: format!("Revert X\n\nThis reverts commit {x}."),
            },
            RawCommit {
                hash: x.to_string(),
                body: "edit X".to_string(),
            },
        ]);
        let mut memo = HashMap::new();
        assert!(!is_applied(x, &reverts, &mut memo));
        assert_eq!(effective_revert_of(x, &reverts), Some(r1.to_string())); // revert this to redo X
    }

    #[test]
    fn categorize_kind_routes_pure_soul_memory_edits_else_code() {
        assert_eq!(
            categorize_kind(&[".hearth/personality.json".to_string()]),
            SelfModKind::Soul
        );
        assert_eq!(
            categorize_kind(&[".hearth/memory.md".to_string()]),
            SelfModKind::Memory
        );
        assert_eq!(
            categorize_kind(&["src/app/chat/ChatView.tsx".to_string()]),
            SelfModKind::Code
        );
        assert_eq!(
            categorize_kind(&[
                ".hearth/personality.json".to_string(),
                "src/x.ts".to_string()
            ]),
            SelfModKind::Code
        ); // mixed → code
        assert_eq!(categorize_kind(&[]), SelfModKind::Code);
    }

    #[test]
    fn a_soul_commit_is_tagged_and_parsed_as_soul() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        run(repo, &["init", "-b", "main"]);
        run(repo, &["config", "user.email", "t@h.dev"]);
        run(repo, &["config", "user.name", "T"]);
        fs::create_dir_all(repo.join(".hearth")).unwrap();
        fs::write(repo.join(".hearth/personality.json"), "{}\n").unwrap();
        commit_self_mod(
            repo,
            &SelfModCommit {
                paths: Some(vec![".hearth/personality.json".to_string()]),
                subject: "personality".to_string(),
                conversation_id: "c1".to_string(),
                kind: None,
                run_id: None,
                subagent: None,
                recovered: false,
            },
        )
        .unwrap();
        let mods = recent_self_mods(repo, 50).unwrap();
        assert_eq!(mods[0].kind, SelfModKind::Soul);
    }

    #[test]
    fn list_dirty_returns_empty_when_clean() {
        let dir = baseline_repo();
        assert!(list_dirty(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn list_dirty_lists_untracked_files() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("new.txt"), "x\n").unwrap();
        assert_eq!(list_dirty(repo).unwrap(), vec!["new.txt".to_string()]);
    }

    #[test]
    fn list_dirty_lists_modified_tracked_files() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("baseline.txt"), "changed\n").unwrap();
        assert_eq!(list_dirty(repo).unwrap(), vec!["baseline.txt".to_string()]);
    }

    #[test]
    fn list_dirty_lists_staged_files() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("staged.txt"), "x\n").unwrap();
        run(repo, &["add", "staged.txt"]);
        assert_eq!(list_dirty(repo).unwrap(), vec!["staged.txt".to_string()]);
    }

    #[test]
    fn list_dirty_handles_paths_with_spaces() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("a file.txt"), "x\n").unwrap();
        assert_eq!(list_dirty(repo).unwrap(), vec!["a file.txt".to_string()]);
    }

    #[test]
    fn list_dirty_lists_multiple_dirty_entries() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("baseline.txt"), "changed\n").unwrap();
        fs::write(repo.join("untracked.txt"), "x\n").unwrap();
        let mut dirty = list_dirty(repo).unwrap();
        dirty.sort();
        assert_eq!(
            dirty,
            vec!["baseline.txt".to_string(), "untracked.txt".to_string()]
        );
    }

    #[test]
    fn list_dirty_handles_a_renamed_staged_entry() {
        let dir = baseline_repo();
        let repo = dir.path();
        // git records a rename as `R  old -> new` in porcelain v1.
        run(repo, &["mv", "baseline.txt", "renamed.txt"]);
        let dirty = list_dirty(repo).unwrap();
        assert_eq!(dirty.len(), 1);
        // We do not require the wrapper to split the rename arrow; we only assert
        // it surfaces the rename without dropping it or mangling it into an empty
        // string.
        assert!(dirty[0].contains("renamed.txt"));
    }

    #[test]
    fn list_renames_surfaces_both_sides_of_a_rename() {
        let dir = baseline_repo();
        let repo = dir.path();
        run(repo, &["mv", "baseline.txt", "renamed.txt"]);
        assert_eq!(
            list_renames(repo).unwrap(),
            vec![Rename {
                from: "baseline.txt".to_string(),
                to: "renamed.txt".to_string(),
            }]
        );
        // A plain edit is not a rename.
        fs::write(repo.join("renamed.txt"), "edited\n").unwrap();
        assert_eq!(
            list_renames(repo).unwrap(),
            vec![Rename {
                from: "baseline.txt".to_string(),
                to: "renamed.txt".to_string(),
            }]
        );
    }

    #[test]
    fn commit_self_mod_creates_a_commit_with_subject_and_trailers() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("feature.txt"), "work\n").unwrap();
        let hash = plain_commit(repo, "add feature", "conv-123");

        assert_eq!(hash, head(repo));
        let body = run(repo, &["log", "-1", "--pretty=%B"]);
        let body = body.trim();
        assert!(body.contains("add feature"));
        assert!(body.contains("Hearth-Conversation: conv-123"));
        assert!(body.contains("Hearth-SelfMod: true"));
    }

    #[test]
    fn commit_self_mod_default_stages_all_dirty_files() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("one.txt"), "1\n").unwrap();
        fs::write(repo.join("two.txt"), "2\n").unwrap();
        plain_commit(repo, "all", "c");

        assert!(list_dirty(repo).unwrap().is_empty());
        let hash = head(repo);
        let mut paths = diff_paths(repo, &hash).unwrap();
        paths.sort();
        assert_eq!(paths, vec!["one.txt".to_string(), "two.txt".to_string()]);
    }

    #[test]
    fn commit_self_mod_paths_option_stages_only_the_specified_files() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("keep.txt"), "k\n").unwrap();
        fs::write(repo.join("leave.txt"), "l\n").unwrap();
        let hash = commit_self_mod(
            repo,
            &SelfModCommit {
                paths: Some(vec!["keep.txt".to_string()]),
                subject: "partial".to_string(),
                conversation_id: "c".to_string(),
                kind: None,
                run_id: None,
                subagent: None,
                recovered: false,
            },
        )
        .unwrap();

        assert_eq!(
            diff_paths(repo, &hash).unwrap(),
            vec!["keep.txt".to_string()]
        );
        // leave.txt remains untracked/dirty.
        assert_eq!(list_dirty(repo).unwrap(), vec!["leave.txt".to_string()]);
    }

    #[test]
    fn recent_self_mods_returns_only_self_mod_commits_newest_first() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("a.txt"), "a\n").unwrap();
        plain_commit(repo, "first selfmod", "conv-A");

        // a plain non-self-mod commit in between
        fs::write(repo.join("manual.txt"), "m\n").unwrap();
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-m", "manual edit"]);

        fs::write(repo.join("b.txt"), "b\n").unwrap();
        plain_commit(repo, "second selfmod", "conv-B");

        let mods = recent_self_mods(repo, 50).unwrap();
        assert_eq!(
            mods.iter().map(|m| m.subject.clone()).collect::<Vec<_>>(),
            vec!["second selfmod".to_string(), "first selfmod".to_string()]
        );
        assert_eq!(
            mods.iter()
                .map(|m| m.conversation_id.clone())
                .collect::<Vec<_>>(),
            vec![Some("conv-B".to_string()), Some("conv-A".to_string())]
        );
        // baseline + manual are excluded
        assert!(mods
            .iter()
            .all(|m| m.subject != "manual edit" && m.subject != "baseline"));
    }

    #[test]
    fn recent_self_mods_conversation_id_is_null_when_the_trailer_is_absent() {
        let dir = baseline_repo();
        let repo = dir.path();
        // Hand-craft a commit that has the SelfMod marker but no conversation trailer.
        fs::write(repo.join("c.txt"), "c\n").unwrap();
        run(repo, &["add", "-A"]);
        run(
            repo,
            &["commit", "-m", "no-conv selfmod\n\nHearth-SelfMod: true"],
        );

        let mods = recent_self_mods(repo, 50).unwrap();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].subject, "no-conv selfmod");
        assert_eq!(mods[0].conversation_id, None);
    }

    #[test]
    fn recent_self_mods_respects_the_limit() {
        let dir = baseline_repo();
        let repo = dir.path();
        for i in 0..3 {
            fs::write(repo.join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
            plain_commit(repo, &format!("mod {i}"), &format!("c{i}"));
        }
        let mods = recent_self_mods(repo, 2).unwrap();
        assert_eq!(mods.len(), 2);
        assert_eq!(
            mods.iter().map(|m| m.subject.clone()).collect::<Vec<_>>(),
            vec!["mod 2".to_string(), "mod 1".to_string()]
        );
    }

    #[test]
    fn revert_commit_reverts_a_specific_commit_and_returns_the_new_head() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("target.txt"), "v1\n").unwrap();
        let target = plain_commit(repo, "add target", "c");

        let before = head(repo);
        let reverted = revert_commit(repo, &target).unwrap();

        assert_eq!(reverted, head(repo));
        assert_ne!(reverted, before);
        // target.txt should be gone again.
        assert!(list_dirty(repo).unwrap().is_empty());
        assert_eq!(
            diff_paths(repo, &reverted).unwrap(),
            vec!["target.txt".to_string()]
        );
    }

    #[test]
    fn revert_commit_reverts_even_when_the_working_tree_is_dirty() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("target.txt"), "v1\n").unwrap();
        let target = plain_commit(repo, "add target", "c");

        // Simulate a bricked boot's leftover state: a modified tracked file plus an
        // untracked file — both would block a plain `git revert`.
        fs::write(repo.join("baseline.txt"), "tampered\n").unwrap();
        fs::write(repo.join("untracked.txt"), "stray\n").unwrap();

        let reverted = revert_commit(repo, &target).unwrap();

        assert_eq!(reverted, head(repo));
        assert!(list_dirty(repo).unwrap().is_empty());
        assert!(!repo.join("untracked.txt").exists()); // clean -fd swept it
        assert!(!repo.join("target.txt").exists()); // the bad commit was undone
        assert_eq!(
            diff_paths(repo, &reverted).unwrap(),
            vec!["target.txt".to_string()]
        );
    }

    #[test]
    fn revert_commit_leaves_later_unrelated_commits_untouched() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("old.txt"), "old\n").unwrap();
        let target = plain_commit(repo, "add old", "c");

        fs::write(repo.join("later.txt"), "later\n").unwrap();
        plain_commit(repo, "add later", "c2");

        revert_commit(repo, &target).unwrap();

        // later.txt must survive the revert of the earlier commit.
        assert!(list_dirty(repo).unwrap().is_empty());
        let subjects = log_subjects(repo);
        assert!(subjects.contains(&"add later".to_string()));
        assert!(subjects.iter().any(|s| s.starts_with("Revert \"add old\"")));
    }

    #[test]
    fn diff_paths_returns_repo_relative_paths_a_commit_changed() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("x.txt"), "x\n").unwrap();
        fs::create_dir(repo.join("sub")).unwrap();
        fs::write(repo.join("sub/y.txt"), "y\n").unwrap();
        let hash = plain_commit(repo, "two files", "c");

        let mut paths = diff_paths(repo, &hash).unwrap();
        paths.sort();
        assert_eq!(paths, vec!["sub/y.txt".to_string(), "x.txt".to_string()]);
    }

    #[test]
    fn diff_paths_works_for_a_revert_commit() {
        let dir = baseline_repo();
        let repo = dir.path();
        fs::write(repo.join("z.txt"), "z\n").unwrap();
        let target = plain_commit(repo, "add z", "c");
        let reverted = revert_commit(repo, &target).unwrap();

        // The revert commit touches the same path the original added.
        assert_eq!(
            diff_paths(repo, &reverted).unwrap(),
            vec!["z.txt".to_string()]
        );
        assert_eq!(
            diff_paths(repo, &target).unwrap(),
            vec!["z.txt".to_string()]
        );
    }

    #[test]
    fn diff_paths_returns_paths_for_the_baseline_root_commit_too() {
        let dir = baseline_repo();
        let repo = dir.path();
        let baseline = run(repo, &["rev-list", "--max-parents=0", "HEAD"])
            .trim()
            .to_string();
        assert_eq!(
            diff_paths(repo, &baseline).unwrap(),
            vec!["baseline.txt".to_string()]
        );
    }
}
