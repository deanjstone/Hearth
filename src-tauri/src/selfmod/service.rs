// Orchestrates the self-modification loop: after the agent writes files, commit
// them and apply the right reload. Will be exposed to IPC in a later chunk.
//
// The agent does the actual file writes (it has the repo as its cwd). This
// service does NOT write source files itself — it observes what changed,
// versions it, and reloads. Keeping the write authority with the agent and the
// safety/versioning here is the separation that makes the loop debuggable.
//
// Ported from electron/main/self-mod/self-mod-service.ts.

use crate::selfmod::git::{
    self, GitError, RevertOutcome, SelfModCommit, SelfModKind, SelfModLogEntry,
};
use crate::selfmod::hmr::HmrController;
use crate::selfmod::path_relevance::{classify_batch, ReloadKind};
use crate::selfmod::reload_driver::ReloadDriver;
use crate::selfmod::run_tracker::{CommitGroup, MAIN_LABEL};
use crate::selfmod::scope_guard::{classify_write, ScopeTier};
use crate::selfmod::validate::TypecheckResult;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Blocking validator for restart-tier edits (W6). Injected so it stays
/// testable — TS's version is `(paths) => Promise<{ok,output}>`; this port's
/// `validate.rs` is synchronous (see its module doc), so the closure is too.
pub type Validator = Box<dyn Fn(&[String]) -> TypecheckResult + Send + Sync>;

/// Arm the boot watchdog with the commit about to trigger a restart (W6).
pub type ArmRestart = Box<dyn Fn(&str) + Send + Sync>;

pub struct BlockedRestart {
    pub output: String,
}

pub struct SelfModResult {
    /// Primary (first) commit hash of the turn — kept for back-compat.
    pub commit: String,
    /// All commit hashes produced this turn (one per file-disjoint subagent group).
    pub commits: Vec<String>,
    pub changed_paths: Vec<String>,
    pub reload: ReloadKind,
    /// Set when a restart-tier edit failed the blocking typecheck — restart
    /// skipped, commit left in place (revertable). The caller surfaces the
    /// failure (W6).
    pub blocked_restart: Option<BlockedRestart>,
    /// Paths the scope guard rejected (secrets / protected island): their
    /// edits were restored on disk and NOT committed (W7 commit-time
    /// enforcement). Empty when nothing was rejected (TS's `rejectedPaths?`
    /// is an absent field; this port always populates the field with an
    /// empty `Vec` instead).
    pub rejected_paths: Vec<String>,
}

/// Optional per-turn run info from the RunTracker (W0/W2).
pub struct TurnRun {
    pub run_id: String,
    pub groups: Vec<CommitGroup>,
}

/// Result of an undo/redo step. A conflict is routed to the agent by the UI.
pub enum StepResult {
    Ok {
        commit: String,
        changed_paths: Vec<String>,
        reload: ReloadKind,
    },
    Dirty,
    Conflict {
        hash: String,
        files: Vec<String>,
    },
    Noop,
}

/// TS's constructor takes `(repoRoot, hmr, validate?, armRestart?)` positionally
/// with two optional trailing collaborators. Rust doesn't have optional
/// positional params, so the two optional collaborators are set via builder
/// methods (`with_validate`/`with_arm_restart`) instead of `Option` args every
/// call site would otherwise have to pass explicitly.
pub struct SelfModService<D: ReloadDriver> {
    repo_root: PathBuf,
    hmr: HmrController<D>,
    validate: Option<Validator>,
    arm_restart: Option<ArmRestart>,
}

impl<D: ReloadDriver> SelfModService<D> {
    pub fn new(repo_root: PathBuf, hmr: HmrController<D>) -> Self {
        Self {
            repo_root,
            hmr,
            validate: None,
            arm_restart: None,
        }
    }

    pub fn with_validate(mut self, validate: Validator) -> Self {
        self.validate = Some(validate);
        self
    }

    pub fn with_arm_restart(mut self, arm_restart: ArmRestart) -> Self {
        self.arm_restart = Some(arm_restart);
        self
    }

    /// Marker file proving a turn started but hasn't been committed yet.
    fn marker_path(&self) -> PathBuf {
        self.repo_root.join(".hearth").join(".turn-in-progress")
    }

    /// The repo's currently-dirty paths — snapshot this BEFORE a turn.
    pub fn dirty_paths(&self) -> Result<Vec<String>, GitError> {
        git::list_dirty(&self.repo_root)
    }

    /// Write the in-progress marker. Call right before the agent's turn starts.
    /// Best-effort, like the rest of this module's marker-file bookkeeping —
    /// unlike TS's `mkdirSync`/`writeFileSync`, which throw on failure, a
    /// failure here is swallowed rather than propagated.
    pub fn begin_turn(&self) {
        let marker = self.marker_path();
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(marker, "1970-01-01T00:00:00.000Z");
    }

    /// Incomplete-run recovery (W0). If a prior turn died before `capture_turn`,
    /// the marker is still present AND the tree is dirty. Commit those orphaned
    /// changes as a `recovered` run so they land in History/undoable — never
    /// silently discarded — then proceed from a clean baseline. A dirty tree
    /// with NO marker is the developer's WIP and is left untouched.
    pub fn recover_if_incomplete(
        &self,
        conversation_id: &str,
    ) -> Result<Option<SelfModResult>, GitError> {
        if !self.marker_path().exists() {
            return Ok(None);
        }
        let dirty = git::list_dirty(&self.repo_root)?;
        if dirty.is_empty() {
            let _ = std::fs::remove_file(self.marker_path());
            return Ok(None);
        }
        let commit = git::commit_self_mod(
            &self.repo_root,
            &SelfModCommit {
                paths: Some(dirty.clone()),
                subject: "recovered: changes from an interrupted turn".to_string(),
                conversation_id: conversation_id.to_string(),
                kind: None,
                run_id: None,
                subagent: None,
                recovered: true,
            },
        )?;
        let _ = std::fs::remove_file(self.marker_path());
        let reload = self.hmr.apply(&dirty);
        Ok(Some(SelfModResult {
            commit: commit.clone(),
            commits: vec![commit],
            changed_paths: dirty,
            reload,
            blocked_restart: None,
            rejected_paths: vec![],
        }))
    }

    /// Call after an agent turn. Commits ONLY paths that became dirty *during*
    /// the turn — `before` is the dirty set captured before the prompt. This is
    /// the critical safety boundary: files the developer was already editing
    /// (or any unrelated dirty state) must NOT be swept into a self-mod commit.
    ///
    /// When `run` is supplied (W2), the turn is split into **one commit per
    /// file-disjoint subagent group**, so a parallel-subagent turn produces
    /// independently-revertable commits grouped by `Hearth-Run`. Any dirty path
    /// the stream missed is reconciled into a `main` group. No `run` (or a
    /// single group) → one-commit behavior. No-op if the turn changed nothing.
    pub fn capture_turn(
        &self,
        conversation_id: &str,
        subject: &str,
        before: &[String],
        run: Option<&TurnRun>,
    ) -> Result<Option<SelfModResult>, GitError> {
        let before_set: BTreeSet<&String> = before.iter().collect();
        let after = git::list_dirty(&self.repo_root)?;
        let all_changed: Vec<String> = after
            .into_iter()
            .filter(|p| !before_set.contains(p))
            .collect();
        let _ = std::fs::remove_file(self.marker_path());
        if all_changed.is_empty() {
            return Ok(None);
        }

        // W7 commit-time scope enforcement. This is the LIVE protection
        // boundary: reject writes to the protected island or the hard-blocked
        // denylist by restoring them on disk and never committing them;
        // canvas paths proceed. By design the protected island has NO
        // approval escape hatch: it stays inviolable regardless of adapter or
        // permission mode.
        let mut rejected: BTreeSet<String> = all_changed
            .iter()
            .filter(|p| classify_write(p, &self.repo_root).tier != ScopeTier::Canvas)
            .cloned()
            .collect();
        // A rename's OLD path is omitted from list_dirty, so `git mv
        // <protected> <canvas>` would commit the island deletion under a
        // canvas name. Reject any rename that touches a non-canvas path on
        // EITHER side, restoring both: the old file is checked out from HEAD,
        // the new one dropped.
        for r in git::list_renames(&self.repo_root)? {
            let ok = classify_write(&r.from, &self.repo_root).tier == ScopeTier::Canvas
                && classify_write(&r.to, &self.repo_root).tier == ScopeTier::Canvas;
            if !ok {
                rejected.insert(r.from);
                rejected.insert(r.to);
            }
        }
        let rejected_paths: Vec<String> = rejected.iter().cloned().collect();
        if !rejected_paths.is_empty() {
            git::restore_paths(&self.repo_root, &rejected_paths);
        }
        let changed_paths: Vec<String> = all_changed
            .into_iter()
            .filter(|p| !rejected.contains(p))
            .collect();
        if changed_paths.is_empty() {
            return Ok(if !rejected_paths.is_empty() {
                Some(SelfModResult {
                    commit: String::new(),
                    commits: vec![],
                    changed_paths: vec![],
                    reload: ReloadKind::Hmr,
                    blocked_restart: None,
                    rejected_paths,
                })
            } else {
                None
            });
        }

        let groups = Self::resolve_groups(&changed_paths, run.map(|r| r.groups.as_slice()));
        let mut commits = Vec::new();
        let multi = groups.len() > 1;
        for g in &groups {
            let subj = if multi && g.subagent_label != MAIN_LABEL {
                format!("{}: {}", g.subagent_label, subject)
            } else {
                subject.to_string()
            };
            commits.push(git::commit_self_mod(
                &self.repo_root,
                &SelfModCommit {
                    paths: Some(g.paths.clone()),
                    subject: subj,
                    conversation_id: conversation_id.to_string(),
                    kind: None,
                    run_id: run.map(|r| r.run_id.clone()),
                    subagent: (g.subagent_label != MAIN_LABEL).then(|| g.subagent_label.clone()),
                    recovered: false,
                },
            )?);
        }

        // W6 blocking gate: a restart-tier edit (main/preload/config) can brick
        // boot, and the renderer crash surface can't recover main. Typecheck
        // BEFORE restart; on failure, keep the current process alive and
        // surface it (commit stays, revertable). Renderer/full-reload tiers
        // are validated async by the caller.
        let tier = classify_batch(&changed_paths);
        if tier == ReloadKind::ProcessRestart {
            if let Some(validate) = &self.validate {
                let tc = validate(&changed_paths);
                if !tc.ok {
                    return Ok(Some(SelfModResult {
                        commit: commits[0].clone(),
                        commits,
                        changed_paths,
                        reload: tier,
                        blocked_restart: Some(BlockedRestart { output: tc.output }),
                        rejected_paths,
                    }));
                }
                // Typecheck passed — arm the boot watchdog so a runtime
                // boot-crash still auto-reverts and relaunches.
                if let Some(arm) = &self.arm_restart {
                    arm(&commits[0]);
                }
            }
        }
        let reload = self.hmr.apply(&changed_paths);
        Ok(Some(SelfModResult {
            commit: commits[0].clone(),
            commits,
            changed_paths,
            reload,
            blocked_restart: None,
            rejected_paths,
        }))
    }

    /// Map the run's tracked groups onto the *actually-dirty* paths, and sweep
    /// any dirty path the stream missed into a `main` group (reconciliation
    /// union). Each returned group is file-disjoint by construction (from the
    /// RunTracker).
    fn resolve_groups(
        changed_paths: &[String],
        groups: Option<&[CommitGroup]>,
    ) -> Vec<CommitGroup> {
        let dirty: BTreeSet<&String> = changed_paths.iter().collect();
        let mut claimed: BTreeSet<String> = BTreeSet::new();
        let mut result = Vec::new();
        for g in groups.unwrap_or(&[]) {
            let paths: Vec<String> = g
                .paths
                .iter()
                .filter(|p| dirty.contains(p))
                .cloned()
                .collect();
            if paths.is_empty() {
                continue;
            }
            for p in &paths {
                claimed.insert(p.clone());
            }
            result.push(CommitGroup {
                paths,
                subagent_label: g.subagent_label.clone(),
                labels: g.labels.clone(),
            });
        }
        let leftover: Vec<String> = changed_paths
            .iter()
            .filter(|p| !claimed.contains(*p))
            .cloned()
            .collect();
        if !leftover.is_empty() {
            result.push(CommitGroup {
                paths: leftover,
                subagent_label: MAIN_LABEL.to_string(),
                labels: vec![MAIN_LABEL.to_string()],
            });
        }
        if result.is_empty() {
            vec![CommitGroup {
                paths: changed_paths.to_vec(),
                subagent_label: MAIN_LABEL.to_string(),
                labels: vec![MAIN_LABEL.to_string()],
            }]
        } else {
            result
        }
    }

    /// Step the history (undo a self-mod, or redo by reverting its revert).
    /// Guards a dirty tree, and on a revert conflict returns the conflicted
    /// files so the UI can hand the resolution to Hearth's agent.
    pub fn undo(&self, hash: &str) -> Result<StepResult, GitError> {
        self.step(hash, hash)
    }

    pub fn redo(&self, hash: &str) -> Result<StepResult, GitError> {
        match git::redo_target(&self.repo_root, hash)? {
            None => Ok(StepResult::Noop),
            Some(target) => self.step(hash, &target),
        }
    }

    fn step(&self, original_hash: &str, revert_hash: &str) -> Result<StepResult, GitError> {
        // Only TRACKED changes block a revert — untracked files are never
        // touched by `git revert`, so an unrelated untracked file must not jam
        // undo/redo.
        if !git::list_tracked_dirty(&self.repo_root)?.is_empty() {
            return Ok(StepResult::Dirty);
        }
        match git::try_revert(&self.repo_root, revert_hash)? {
            RevertOutcome::Conflict { files } => Ok(StepResult::Conflict {
                hash: original_hash.to_string(),
                files,
            }),
            RevertOutcome::Reverted { commit } => {
                let changed_paths = git::diff_paths(&self.repo_root, &commit)?;
                let reload = self.hmr.apply(&changed_paths);
                Ok(StepResult::Ok {
                    commit,
                    changed_paths,
                    reload,
                })
            }
        }
    }

    /// Commit specific repo paths directly (not via an agent turn) with an
    /// explicit surface category — used by Settings to version soul/memory
    /// edits. No HMR: soul/memory are instruction data, not renderer source.
    pub fn commit_managed(
        &self,
        paths: &[String],
        subject: &str,
        kind: SelfModKind,
    ) -> Result<(String, Vec<String>), GitError> {
        let commit = git::commit_self_mod(
            &self.repo_root,
            &SelfModCommit {
                paths: Some(paths.to_vec()),
                subject: subject.to_string(),
                conversation_id: "settings".to_string(),
                kind: Some(kind),
                run_id: None,
                subagent: None,
                recovered: false,
            },
        )?;
        Ok((commit, paths.to_vec()))
    }

    pub fn history(&self) -> Result<Vec<SelfModLogEntry>, GitError> {
        git::recent_self_mods(&self.repo_root, 50)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::rc::Rc;
    use tempfile::{tempdir, TempDir};

    fn run(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("failed to spawn git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn write(repo: &Path, rel: &str, content: &str) {
        let p = repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn baseline_repo() -> TempDir {
        let dir = tempdir().expect("tempdir");
        let repo = dir.path();
        run(repo, &["init", "-b", "main"]);
        run(repo, &["config", "user.email", "test@hearth.dev"]);
        run(repo, &["config", "user.name", "Hearth Test"]);
        run(repo, &["config", "commit.gpgsign", "false"]);
        // Matches the real repo's ignore rules for the runtime marker dir this
        // service reads/writes — without it, `.hearth/.turn-in-progress` shows
        // up as an ordinary dirty path and leaks into commits under test.
        write(repo, ".gitignore", ".hearth/\n");
        write(
            repo,
            "src/app/chat/ChatApp.tsx",
            "export const title = \"Hearth\"\n",
        );
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-m", "baseline"]);
        dir
    }

    /// Records what the HMR controller asked for, so tests can assert the tier
    /// without a real window.
    #[derive(Default)]
    struct RecordingDriver {
        calls: RefCell<Vec<&'static str>>,
    }

    impl ReloadDriver for RecordingDriver {
        fn reload_window(&self) {
            self.calls.borrow_mut().push("reload");
        }
        fn restart_app(&self) {
            self.calls.borrow_mut().push("restart");
        }
        fn supports_covered_reload(&self) -> bool {
            true
        }
        fn covered_reload(&self) {
            self.calls.borrow_mut().push("covered");
        }
    }

    // `HmrController` owns its driver by value, but tests need to inspect the
    // driver's recorded calls *after* it's been moved into a `SelfModService`.
    // Implementing `ReloadDriver` for `Rc<RecordingDriver>` (instead of handing
    // the controller a bare `RecordingDriver`) lets the controller and the test
    // hold clones of the same `Rc`, sharing the `RefCell` call log underneath —
    // no change to `HmrController`/`SelfModService` needed.
    impl ReloadDriver for Rc<RecordingDriver> {
        fn reload_window(&self) {
            RecordingDriver::reload_window(self)
        }
        fn restart_app(&self) {
            RecordingDriver::restart_app(self)
        }
        fn supports_covered_reload(&self) -> bool {
            RecordingDriver::supports_covered_reload(self)
        }
        fn covered_reload(&self) {
            RecordingDriver::covered_reload(self)
        }
    }

    fn svc_with_vite(
        repo: &Path,
        vite_served: bool,
    ) -> (SelfModService<Rc<RecordingDriver>>, Rc<RecordingDriver>) {
        let driver = Rc::new(RecordingDriver::default());
        let hmr = HmrController::new(driver.clone(), vite_served);
        (SelfModService::new(repo.to_path_buf(), hmr), driver)
    }

    fn svc(repo: &Path) -> (SelfModService<Rc<RecordingDriver>>, Rc<RecordingDriver>) {
        svc_with_vite(repo, false)
    }

    fn commit_subjects(repo: &Path) -> Vec<String> {
        run(repo, &["log", "--format=%s"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn commit_body(repo: &Path, hash: &str) -> String {
        run(repo, &["show", "-s", "--format=%B", hash])
    }

    // ---- captureTurn ----

    #[test]
    fn no_changes_returns_none() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        let result = service
            .capture_turn("conv1", "edit", &before, None)
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn hmr_tier_commits_with_trailers() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/app/chat/ChatApp.tsx",
            "export const title = \"Hearth 2\"\n",
        );
        let result = service
            .capture_turn("conv1", "tweak title", &before, None)
            .unwrap()
            .expect("expected a commit");
        assert_eq!(result.reload, ReloadKind::Hmr);
        assert_eq!(
            result.changed_paths,
            vec!["src/app/chat/ChatApp.tsx".to_string()]
        );
        assert!(
            driver.calls.borrow().is_empty(),
            "Hmr tier must not touch the driver"
        );
        let body = commit_body(repo, &result.commit);
        assert!(body.starts_with("tweak title"));
        assert!(body.contains("Hearth-Conversation: conv1"));
        assert!(body.contains("Hearth-Kind: code"));
        assert!(body.contains("Hearth-SelfMod: true"));
    }

    #[test]
    fn route_full_reload_triggers_plain_reload_when_not_vite_served() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, driver) = svc_with_vite(repo, false);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/routes/chat.tsx",
            "export default function Chat() { return null }\n",
        );
        let result = service
            .capture_turn("conv1", "add route", &before, None)
            .unwrap()
            .expect("expected a commit");
        assert_eq!(result.reload, ReloadKind::FullReload);
        assert_eq!(driver.calls.borrow().as_slice(), &["reload"]);
    }

    #[test]
    fn vite_served_full_reload_uses_covered_reload() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, driver) = svc_with_vite(repo, true);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/routes/chat.tsx",
            "export default function Chat() { return null }\n",
        );
        let result = service
            .capture_turn("conv1", "add route", &before, None)
            .unwrap()
            .expect("expected a commit");
        assert_eq!(result.reload, ReloadKind::FullReload);
        assert_eq!(driver.calls.borrow().as_slice(), &["covered"]);
    }

    #[test]
    fn only_paths_dirtied_during_the_turn_are_committed() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        // Developer's pre-existing, unrelated WIP.
        write(repo, "src/app/scratch.ts", "// wip\n");
        let before = service.dirty_paths().unwrap();
        assert_eq!(before, vec!["src/app/scratch.ts".to_string()]);

        write(
            repo,
            "src/app/chat/ChatApp.tsx",
            "export const title = \"Hearth 2\"\n",
        );
        let result = service
            .capture_turn("conv1", "tweak title", &before, None)
            .unwrap()
            .expect("expected a commit");
        assert_eq!(
            result.changed_paths,
            vec!["src/app/chat/ChatApp.tsx".to_string()]
        );

        // The pre-existing WIP file must still be dirty, not swept into the commit.
        assert_eq!(
            service.dirty_paths().unwrap(),
            vec!["src/app/scratch.ts".to_string()]
        );
    }

    // ---- undo ----

    #[test]
    fn undo_reverts_content() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/app/chat/ChatApp.tsx",
            "export const title = \"Hearth 2\"\n",
        );
        let result = service
            .capture_turn("conv1", "tweak title", &before, None)
            .unwrap()
            .unwrap();

        match service.undo(&result.commit).unwrap() {
            StepResult::Ok { .. } => {}
            _ => panic!("expected Ok"),
        }
        let content = fs::read_to_string(repo.join("src/app/chat/ChatApp.tsx")).unwrap();
        assert_eq!(content, "export const title = \"Hearth\"\n");
    }

    #[test]
    fn undo_escalates_reload_tier() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, driver) = svc_with_vite(repo, false);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/routes/chat.tsx",
            "export default function Chat() { return null }\n",
        );
        let result = service
            .capture_turn("conv1", "add route", &before, None)
            .unwrap()
            .unwrap();
        driver.calls.borrow_mut().clear();

        match service.undo(&result.commit).unwrap() {
            StepResult::Ok { reload, .. } => assert_eq!(reload, ReloadKind::FullReload),
            _ => panic!("expected Ok"),
        }
        assert_eq!(driver.calls.borrow().as_slice(), &["reload"]);
    }

    // ---- history ----

    #[test]
    fn history_lists_with_conversation_id() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/app/chat/ChatApp.tsx",
            "export const title = \"Hearth 2\"\n",
        );
        let result = service
            .capture_turn("conv-xyz", "tweak title", &before, None)
            .unwrap()
            .unwrap();

        let history = service.history().unwrap();
        let entry = history.iter().find(|e| e.hash == result.commit).unwrap();
        assert_eq!(entry.conversation_id.as_deref(), Some("conv-xyz"));
        assert!(!entry.reverted);
    }

    #[test]
    fn history_marks_reverted_flag_after_undo() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/app/chat/ChatApp.tsx",
            "export const title = \"Hearth 2\"\n",
        );
        let result = service
            .capture_turn("conv1", "tweak title", &before, None)
            .unwrap()
            .unwrap();
        service.undo(&result.commit).unwrap();

        let history = service.history().unwrap();
        let entry = history.iter().find(|e| e.hash == result.commit).unwrap();
        assert!(entry.reverted);
    }

    // ---- per-subagent commits (W2) ----

    #[test]
    fn multi_group_produces_one_commit_per_group() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/shell/Rail.tsx",
            "export const Rail = () => null\n",
        );
        write(
            repo,
            "src/shell/Topbar.tsx",
            "export const Topbar = () => null\n",
        );
        let run_info = TurnRun {
            run_id: "run1".to_string(),
            groups: vec![
                CommitGroup {
                    paths: vec!["src/shell/Rail.tsx".to_string()],
                    subagent_label: "Left sidebar".to_string(),
                    labels: vec!["taskA".to_string()],
                },
                CommitGroup {
                    paths: vec!["src/shell/Topbar.tsx".to_string()],
                    subagent_label: "Heading".to_string(),
                    labels: vec!["taskB".to_string()],
                },
            ],
        };
        let result = service
            .capture_turn("conv1", "parallel edit", &before, Some(&run_info))
            .unwrap()
            .unwrap();
        assert_eq!(result.commits.len(), 2);

        let subjects = commit_subjects(repo);
        assert!(subjects.iter().any(|s| s.starts_with("Left sidebar:")));
        assert!(subjects.iter().any(|s| s.starts_with("Heading:")));

        // Each commit must only touch its own group's file.
        for hash in &result.commits {
            let files = run(repo, &["show", "--stat", "--format=", hash]);
            if files.contains("Rail.tsx") {
                assert!(!files.contains("Topbar.tsx"));
            } else {
                assert!(files.contains("Topbar.tsx"));
            }
        }
    }

    #[test]
    fn a_dirty_path_missing_from_every_group_is_reconciled_to_main() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/shell/Rail.tsx",
            "export const Rail = () => null\n",
        );
        write(repo, "src/shell/store.ts", "export const store = {}\n");
        let run_info = TurnRun {
            run_id: "run1".to_string(),
            groups: vec![CommitGroup {
                paths: vec!["src/shell/Rail.tsx".to_string()],
                subagent_label: "Left sidebar".to_string(),
                labels: vec!["taskA".to_string()],
            }],
        };
        let result = service
            .capture_turn("conv1", "edit", &before, Some(&run_info))
            .unwrap()
            .unwrap();
        assert_eq!(result.commits.len(), 2);
        let subjects = commit_subjects(repo);
        assert!(subjects.iter().any(|s| s.starts_with("Left sidebar:")));
        // The path no group claimed lands in a plain "main" commit, unprefixed.
        assert!(subjects.iter().any(|s| s == "edit"));
    }

    #[test]
    fn incomplete_run_is_recovered_as_its_own_commit() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        service.begin_turn();
        write(
            repo,
            "src/app/chat/ChatApp.tsx",
            "export const title = \"crashed mid turn\"\n",
        );

        let result = service
            .recover_if_incomplete("conv-recovered")
            .unwrap()
            .expect("expected a recovery commit");
        assert_eq!(
            result.changed_paths,
            vec!["src/app/chat/ChatApp.tsx".to_string()]
        );
        assert!(!service.marker_path().exists());
        let body = commit_body(repo, &result.commit);
        assert!(body.contains("Hearth-Recovered: true"));
    }

    #[test]
    fn dirty_tree_with_no_marker_is_left_as_wip() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        // No begin_turn() call — this is ordinary developer WIP, marker absent.
        write(
            repo,
            "src/app/chat/ChatApp.tsx",
            "export const title = \"still editing\"\n",
        );

        let result = service.recover_if_incomplete("conv1").unwrap();
        assert!(result.is_none());
        assert_eq!(
            service.dirty_paths().unwrap(),
            vec!["src/app/chat/ChatApp.tsx".to_string()]
        );
    }

    // ---- W7 commit-time scope enforcement ----

    #[test]
    fn protected_island_write_is_rejected_canvas_write_still_committed() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        write(
            repo,
            "src/app/chat/ChatApp.tsx",
            "export const title = \"Hearth 2\"\n",
        );
        write(repo, "src-tauri/src/selfmod/new_module.rs", "// sneaky\n");

        let result = service
            .capture_turn("conv1", "edit", &before, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            result.rejected_paths,
            vec!["src-tauri/src/selfmod/new_module.rs".to_string()]
        );
        assert_eq!(
            result.changed_paths,
            vec!["src/app/chat/ChatApp.tsx".to_string()]
        );
        assert!(!repo.join("src-tauri/src/selfmod/new_module.rs").exists());
    }

    #[test]
    fn editing_an_existing_protected_file_is_reverted_on_disk() {
        let dir = baseline_repo();
        let repo = dir.path();
        write(repo, "src-tauri/src/selfmod/existing.rs", "// original\n");
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-m", "seed protected file"]);

        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        write(repo, "src-tauri/src/selfmod/existing.rs", "// tampered\n");

        let result = service
            .capture_turn("conv1", "edit", &before, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            result.rejected_paths,
            vec!["src-tauri/src/selfmod/existing.rs".to_string()]
        );
        assert!(result.changed_paths.is_empty());
        let content = fs::read_to_string(repo.join("src-tauri/src/selfmod/existing.rs")).unwrap();
        assert_eq!(content, "// original\n");
    }

    #[test]
    fn renaming_a_protected_file_into_canvas_rejects_both_sides() {
        let dir = baseline_repo();
        let repo = dir.path();
        write(repo, "src-tauri/src/selfmod/secret.rs", "// protected\n");
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-m", "seed protected file"]);

        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        run(
            repo,
            &["mv", "src-tauri/src/selfmod/secret.rs", "src/secret.rs"],
        );

        let result = service
            .capture_turn("conv1", "smuggle", &before, None)
            .unwrap()
            .unwrap();
        let mut rejected = result.rejected_paths.clone();
        rejected.sort();
        assert_eq!(
            rejected,
            vec![
                "src-tauri/src/selfmod/secret.rs".to_string(),
                "src/secret.rs".to_string(),
            ]
        );
        assert!(result.changed_paths.is_empty());
        assert!(repo.join("src-tauri/src/selfmod/secret.rs").exists());
        assert!(!repo.join("src/secret.rs").exists());
    }

    #[test]
    fn a_turn_with_only_a_blocked_write_commits_nothing() {
        let dir = baseline_repo();
        let repo = dir.path();
        let (service, _driver) = svc(repo);
        let before = service.dirty_paths().unwrap();
        write(repo, ".env", "SECRET=1\n");

        let result = service
            .capture_turn("conv1", "edit", &before, None)
            .unwrap()
            .unwrap();
        assert_eq!(result.rejected_paths, vec![".env".to_string()]);
        assert!(result.changed_paths.is_empty());
        assert!(result.commits.is_empty());
        assert!(!repo.join(".env").exists());
    }
}
