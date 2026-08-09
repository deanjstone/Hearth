// Run tracker (W0). A "run" = one agent turn (one host.prompt), keyed by run_id +
// session_id. It records, per run:
//   - which paths were written and by which subagent (attribution from the ACP
//     parent tool-call id, or MAIN_LABEL for the orchestrator's own edits),
//   - per-write baselines (the pre-edit content, when known),
//   - the live Task lanes (subagent tool-calls) and their status, which gives the
//     in-flight writer count that drives W1's concurrency gate.
//
// At end_run it partitions the run's writes into **file-disjoint connected
// components** of the (subagent <-> file) graph: subagents that share a file merge
// into one group, guaranteeing no two commit groups touch the same file. Each
// group becomes one per-subagent commit in W2.
//
// Pure data structure — no git/fs — so it unit-tests without a repo.
//
// Ported from electron/main/self-mod/run-tracker.ts. That file keys internal
// maps with JS `Map`, which iterates in insertion order; Rust's `HashMap` makes
// no such guarantee. This port uses `BTreeMap`/`BTreeSet` instead (sorted-by-key
// iteration) so output stays deterministic — the TS test suite already
// `.sort()`s before comparing collections that could vary by writer order, so
// sorted-instead-of-insertion order is a safe, behavior-preserving swap.

use std::collections::{BTreeMap, BTreeSet};

pub const MAIN_LABEL: &str = "main";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneStatus {
    Pending,
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone)]
struct Lane {
    tool_call_id: String,
    title: String,
    status: LaneStatus,
}

#[derive(Debug, Clone)]
struct WriteRecord {
    /// Subagent label: the parent tool-call id, or MAIN_LABEL for the orchestrator.
    label: String,
    /// Pre-edit content if known (else None -> caller may backfill from git HEAD).
    baseline: Option<String>,
}

struct RunState {
    session_id: String,
    /// repo-relative path -> write record (latest wins; label is first writer's).
    writes: BTreeMap<String, WriteRecord>,
    /// tool-call id -> lane (every tool-call seen; only subagent ones are surfaced).
    lanes: BTreeMap<String, Lane>,
    /// Tool-call ids known to be subagents (a Task — i.e. the parent of other calls).
    /// Only these surface as lanes / count toward the concurrency gate, so an
    /// orchestrator's own Read/Edit/Bash calls don't masquerade as subagents.
    subagents: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitGroup {
    /// repo-relative paths in this file-disjoint component.
    pub paths: Vec<String>,
    /// Human label for the commit subject (subagent title(s), or MAIN_LABEL).
    pub subagent_label: String,
    /// The raw attribution labels that merged into this group.
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityLane {
    pub tool_call_id: String,
    pub title: String,
    pub status: LaneStatus,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunActivity {
    pub run_id: String,
    pub session_id: String,
    pub lanes: Vec<ActivityLane>,
    /// paths written by more than one distinct subagent (collision warning).
    pub collisions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndRunResult {
    pub session_id: String,
    pub groups: Vec<CommitGroup>,
}

#[derive(Default)]
pub struct RunTracker {
    runs: BTreeMap<String, RunState>,
    session_to_run: BTreeMap<String, String>,
}

impl RunTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin_run(&mut self, run_id: &str, session_id: &str) {
        self.runs.insert(
            run_id.to_string(),
            RunState {
                session_id: session_id.to_string(),
                writes: BTreeMap::new(),
                lanes: BTreeMap::new(),
                subagents: BTreeSet::new(),
            },
        );
        self.session_to_run
            .insert(session_id.to_string(), run_id.to_string());
    }

    /// Mark a tool-call id as a subagent (Task) — discovered when another call reports
    /// it as its parent. Only marked ids surface as lanes / count toward concurrency.
    pub fn mark_subagent(&mut self, run_id: &str, tool_call_id: &str) {
        if let Some(run) = self.runs.get_mut(run_id) {
            run.subagents.insert(tool_call_id.to_string());
        }
    }

    /// The active run for a session, or None. Used to route streamed updates.
    pub fn run_for_session(&self, session_id: &str) -> Option<String> {
        self.session_to_run.get(session_id).cloned()
    }

    /// Record a write. `parent_tool_call_id` attributes it to a subagent; None ->
    /// the orchestrator (MAIN_LABEL). The first writer of a path owns its label;
    /// a later writer by a *different* subagent marks a collision (surfaced in
    /// activity) but does not change the label — file-disjoint grouping handles it.
    pub fn record_write(
        &mut self,
        run_id: &str,
        repo_rel_path: &str,
        parent_tool_call_id: Option<&str>,
        baseline: Option<&str>,
    ) {
        let Some(run) = self.runs.get_mut(run_id) else {
            return;
        };
        let label = parent_tool_call_id
            .filter(|s| !s.is_empty())
            .unwrap_or(MAIN_LABEL)
            .to_string();
        if let Some(existing) = run.writes.get_mut(repo_rel_path) {
            // Keep first label; backfill baseline if newly known.
            if existing.baseline.is_none() {
                if let Some(b) = baseline {
                    existing.baseline = Some(b.to_string());
                }
            }
            // Track that a second distinct subagent touched this path.
            if existing.label != label {
                existing.label = format!("{} {}", existing.label, label);
            }
            return;
        }
        run.writes.insert(
            repo_rel_path.to_string(),
            WriteRecord {
                label,
                baseline: baseline.map(|s| s.to_string()),
            },
        );
    }

    /// Record / update a subagent Task lane.
    pub fn record_lane(
        &mut self,
        run_id: &str,
        tool_call_id: &str,
        title: &str,
        status: LaneStatus,
    ) {
        let Some(run) = self.runs.get_mut(run_id) else {
            return;
        };
        if let Some(lane) = run.lanes.get_mut(tool_call_id) {
            lane.status = status;
            if !title.is_empty() {
                lane.title = title.to_string();
            }
        } else {
            run.lanes.insert(
                tool_call_id.to_string(),
                Lane {
                    tool_call_id: tool_call_id.to_string(),
                    title: if title.is_empty() {
                        "Subagent".to_string()
                    } else {
                        title.to_string()
                    },
                    status,
                },
            );
        }
    }

    /// Number of subagent lanes currently writing (pending|running). When >=2 we are
    /// in parallel-subagent mode and W1's overlay should pin (atomic apply). A lone
    /// subagent or the orchestrator alone stays single-writer (immediate HMR).
    pub fn concurrent_writer_count(&self, run_id: &str) -> usize {
        let Some(run) = self.runs.get(run_id) else {
            return 0;
        };
        run.lanes
            .values()
            .filter(|l| run.subagents.contains(&l.tool_call_id))
            .filter(|l| matches!(l.status, LaneStatus::Pending | LaneStatus::Running))
            .count()
    }

    pub fn is_concurrent(&self, run_id: &str) -> bool {
        self.concurrent_writer_count(run_id) >= 2
    }

    /// repo-relative paths written so far this run (for overlay pin discovery).
    pub fn written_paths(&self, run_id: &str) -> Vec<String> {
        self.runs
            .get(run_id)
            .map(|r| r.writes.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn baseline_for(&self, run_id: &str, repo_rel_path: &str) -> Option<String> {
        self.runs
            .get(run_id)?
            .writes
            .get(repo_rel_path)?
            .baseline
            .clone()
    }

    /// Live activity snapshot for the W4 panel.
    pub fn activity(&self, run_id: &str) -> Option<RunActivity> {
        let run = self.runs.get(run_id)?;
        let mut lane_paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut collisions: Vec<String> = Vec::new();
        for (p, rec) in &run.writes {
            let labels: Vec<&str> = rec.label.split(' ').collect();
            if labels.len() > 1 {
                collisions.push(p.clone());
            }
            for label in labels {
                if label == MAIN_LABEL {
                    continue;
                }
                lane_paths
                    .entry(label.to_string())
                    .or_default()
                    .push(p.clone());
            }
        }
        let lanes: Vec<ActivityLane> = run
            .lanes
            .values()
            .filter(|l| run.subagents.contains(&l.tool_call_id))
            .map(|l| ActivityLane {
                tool_call_id: l.tool_call_id.clone(),
                title: l.title.clone(),
                status: l.status,
                paths: lane_paths.get(&l.tool_call_id).cloned().unwrap_or_default(),
            })
            .collect();
        Some(RunActivity {
            run_id: run_id.to_string(),
            session_id: run.session_id.clone(),
            lanes,
            collisions,
        })
    }

    /// Finalize the run: partition writes into file-disjoint connected components
    /// of the (subagent <-> file) graph. Returns one group per component.
    pub fn end_run(&mut self, run_id: &str) -> Option<EndRunResult> {
        let run = self.runs.remove(run_id)?;
        if self.session_to_run.get(&run.session_id).map(|s| s.as_str()) == Some(run_id) {
            self.session_to_run.remove(&run.session_id);
        }
        let groups = Self::group(&run);
        Some(EndRunResult {
            session_id: run.session_id,
            groups,
        })
    }

    /// Build file-disjoint components via union-find (path compression) over labels and paths.
    fn group(run: &RunState) -> Vec<CommitGroup> {
        let mut parent: BTreeMap<String, String> = BTreeMap::new();

        fn find(parent: &mut BTreeMap<String, String>, x: &str) -> String {
            let mut r = x.to_string();
            while parent.get(&r) != Some(&r) {
                r = parent.get(&r).expect("root has a parent entry").clone();
            }
            let mut c = x.to_string();
            while parent.get(&c) != Some(&r) {
                let next = parent.get(&c).expect("node has a parent entry").clone();
                parent.insert(c.clone(), r.clone());
                c = next;
            }
            r
        }
        fn ensure(parent: &mut BTreeMap<String, String>, x: &str) {
            parent.entry(x.to_string()).or_insert_with(|| x.to_string());
        }
        fn union(parent: &mut BTreeMap<String, String>, a: &str, b: &str) {
            ensure(parent, a);
            ensure(parent, b);
            let ra = find(parent, a);
            let rb = find(parent, b);
            parent.insert(ra, rb);
        }

        for (p, rec) in &run.writes {
            let path_node = format!("p:{p}");
            ensure(&mut parent, &path_node);
            for label in rec.label.split(' ') {
                union(&mut parent, &path_node, &format!("l:{label}"));
            }
        }

        let mut by_root: BTreeMap<String, (Vec<String>, BTreeSet<String>)> = BTreeMap::new();
        for (p, rec) in &run.writes {
            let root = find(&mut parent, &format!("p:{p}"));
            let entry = by_root.entry(root).or_default();
            entry.0.push(p.clone());
            for label in rec.label.split(' ') {
                entry.1.insert(label.to_string());
            }
        }

        let lane_title = |label: &str| -> String {
            if label == MAIN_LABEL {
                return MAIN_LABEL.to_string();
            }
            run.lanes
                .get(label)
                .map(|l| l.title.clone())
                .unwrap_or_else(|| label.to_string())
        };

        let mut groups: Vec<CommitGroup> = Vec::new();
        for (mut paths, labels) in by_root.into_values() {
            let label_list: Vec<String> = labels.into_iter().collect();
            let titles: Vec<String> = label_list
                .iter()
                .map(|l| lane_title(l))
                .filter(|t| t.as_str() != MAIN_LABEL)
                .collect();
            let subagent_label = if !titles.is_empty() {
                titles.join(" + ")
            } else {
                MAIN_LABEL.to_string()
            };
            paths.sort();
            groups.push(CommitGroup {
                paths,
                subagent_label,
                labels: label_list,
            });
        }
        // Deterministic order: by the first path in each group.
        groups.sort_by(|a, b| {
            a.paths
                .first()
                .cloned()
                .unwrap_or_default()
                .cmp(&b.paths.first().cloned().unwrap_or_default())
        });
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- attribution + grouping ---

    #[test]
    fn disjoint_subagents_yield_one_group_each() {
        let mut t = RunTracker::new();
        t.begin_run("r1", "s1");
        t.record_lane("r1", "taskA", "Left sidebar", LaneStatus::Running);
        t.record_lane("r1", "taskB", "Heading", LaneStatus::Running);
        t.record_write("r1", "src/shell/Rail.tsx", Some("taskA"), None);
        t.record_write("r1", "src/shell/Rail.css", Some("taskA"), None);
        t.record_write("r1", "src/shell/Topbar.tsx", Some("taskB"), None);

        let res = t.end_run("r1").unwrap();
        assert_eq!(res.groups.len(), 2);
        let by_label: BTreeMap<String, Vec<String>> = res
            .groups
            .iter()
            .map(|g| (g.subagent_label.clone(), g.paths.clone()))
            .collect();
        assert_eq!(
            by_label["Left sidebar"],
            vec![
                "src/shell/Rail.css".to_string(),
                "src/shell/Rail.tsx".to_string()
            ]
        );
        assert_eq!(
            by_label["Heading"],
            vec!["src/shell/Topbar.tsx".to_string()]
        );
    }

    #[test]
    fn subagents_sharing_a_file_merge_into_one_group() {
        let mut t = RunTracker::new();
        t.begin_run("r1", "s1");
        t.record_lane("r1", "taskA", "Heading", LaneStatus::Running);
        t.record_lane("r1", "taskB", "Right sidebar", LaneStatus::Running);
        // both touch __root.tsx
        t.record_write("r1", "src/routes/__root.tsx", Some("taskA"), None);
        t.record_write("r1", "src/routes/__root.tsx", Some("taskB"), None);
        t.record_write("r1", "src/shell/store.ts", Some("taskB"), None);

        let res = t.end_run("r1").unwrap();
        // the shared file collapses taskA + taskB into a single group
        assert_eq!(res.groups.len(), 1);
        assert_eq!(
            res.groups[0].paths,
            vec![
                "src/routes/__root.tsx".to_string(),
                "src/shell/store.ts".to_string()
            ]
        );
        let mut labels = res.groups[0].labels.clone();
        labels.sort();
        assert_eq!(labels, vec!["taskA".to_string(), "taskB".to_string()]);
    }

    #[test]
    fn orchestrator_only_edits_yield_a_single_main_group() {
        let mut t = RunTracker::new();
        t.begin_run("r1", "s1");
        t.record_write("r1", "src/a.ts", None, None);
        t.record_write("r1", "src/b.ts", None, None);
        let res = t.end_run("r1").unwrap();
        assert_eq!(res.groups.len(), 1);
        assert_eq!(res.groups[0].subagent_label, MAIN_LABEL);
        assert_eq!(
            res.groups[0].paths,
            vec!["src/a.ts".to_string(), "src/b.ts".to_string()]
        );
    }

    // --- concurrency gate ---

    #[test]
    fn two_running_lanes_are_concurrent() {
        let mut t = RunTracker::new();
        t.begin_run("r1", "s1");
        t.record_lane("r1", "taskA", "A", LaneStatus::Running);
        t.mark_subagent("r1", "taskA");
        assert!(!t.is_concurrent("r1"));
        t.record_lane("r1", "taskB", "B", LaneStatus::Running);
        t.mark_subagent("r1", "taskB");
        assert!(t.is_concurrent("r1"));
        assert_eq!(t.concurrent_writer_count("r1"), 2);
        // one finishes -> back below threshold
        t.record_lane("r1", "taskA", "A", LaneStatus::Done);
        assert!(!t.is_concurrent("r1"));
    }

    #[test]
    fn a_lone_subagent_is_not_concurrent() {
        let mut t = RunTracker::new();
        t.begin_run("r1", "s1");
        t.record_lane("r1", "taskA", "A", LaneStatus::Running);
        t.mark_subagent("r1", "taskA");
        assert!(!t.is_concurrent("r1"));
    }

    #[test]
    fn unmarked_tool_calls_never_count_as_subagents() {
        let mut t = RunTracker::new();
        t.begin_run("r1", "s1");
        // Two plain top-level tool-calls, neither marked a subagent.
        t.record_lane("r1", "read1", "Read", LaneStatus::Running);
        t.record_lane("r1", "edit1", "Edit", LaneStatus::Running);
        assert_eq!(t.concurrent_writer_count("r1"), 0);
        assert!(!t.is_concurrent("r1"));
        assert!(t.activity("r1").unwrap().lanes.is_empty());
    }

    // --- baseline + activity ---

    #[test]
    fn baseline_is_captured_and_backfilled_new_file_is_none() {
        let mut t = RunTracker::new();
        t.begin_run("r1", "s1");
        t.record_write("r1", "src/a.ts", None, Some("OLD"));
        t.record_write("r1", "src/new.ts", None, None); // new file, no baseline
        assert_eq!(t.baseline_for("r1", "src/a.ts"), Some("OLD".to_string()));
        assert_eq!(t.baseline_for("r1", "src/new.ts"), None);
        // backfill on a later write
        t.record_write("r1", "src/new.ts", None, Some(""));
        assert_eq!(t.baseline_for("r1", "src/new.ts"), Some(String::new()));
    }

    #[test]
    fn activity_reports_lanes_and_collisions() {
        let mut t = RunTracker::new();
        t.begin_run("r1", "s1");
        t.record_lane("r1", "taskA", "A", LaneStatus::Running);
        t.record_lane("r1", "taskB", "B", LaneStatus::Running);
        t.mark_subagent("r1", "taskA");
        t.mark_subagent("r1", "taskB");
        t.record_write("r1", "src/shared.ts", Some("taskA"), None);
        t.record_write("r1", "src/shared.ts", Some("taskB"), None);
        t.record_write("r1", "src/a.ts", Some("taskA"), None);
        let a = t.activity("r1").unwrap();
        assert_eq!(a.collisions, vec!["src/shared.ts".to_string()]);
        let lane_a = a.lanes.iter().find(|l| l.tool_call_id == "taskA").unwrap();
        let mut paths = lane_a.paths.clone();
        paths.sort();
        assert_eq!(
            paths,
            vec!["src/a.ts".to_string(), "src/shared.ts".to_string()]
        );
    }

    #[test]
    fn run_for_session_routing() {
        let mut t = RunTracker::new();
        t.begin_run("r1", "s1");
        assert_eq!(t.run_for_session("s1"), Some("r1".to_string()));
        t.end_run("r1");
        assert_eq!(t.run_for_session("s1"), None);
    }
}
