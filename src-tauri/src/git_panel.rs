// Pure parsing logic for the Git panel / Review tab (Phase 7, tracking issue
// #27). Ported from `electron/main/self-mod/git-ops.ts`'s `parseStatus` and
// `electron/main/self-mod/git-diff.ts`'s `parseUnifiedDiff`. Deliberately
// separate from `selfmod::git` — that module exists purely for the self-mod
// undo/redo/history feature (a different, narrower shell-out surface); this
// one is a general git-panel UI, reusing only the shell-out *pattern*
// (`Command::new("git")`, argv array, no shell), not its types.

use serde::Serialize;
use std::path::Path;
use std::process::Command;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FileTag {
    New,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatusFile {
    pub path: String,
    pub old_path: Option<String>,
    pub tag: FileTag,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<StatusFile>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct BranchInfo {
    pub current: Option<String>,
    pub branches: Vec<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrResult {
    pub created: bool,
    pub detail: String,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DiffTag {
    New,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DiffRowKind {
    Add,
    Del,
    Ctx,
    Hunk,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DiffRow {
    pub t: DiffRowKind,
    pub code: String,
    pub ln: Option<i64>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub file: String,
    pub old_path: Option<String>,
    pub tag: DiffTag,
    pub add: i64,
    pub del: i64,
    pub rows: Vec<DiffRow>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub files: Vec<DiffFile>,
    pub add: i64,
    pub del: i64,
    pub branch: Option<String>,
}

// --- shell-out helpers -----------------------------------------------------

fn git(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed ({}): {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// --- status ------------------------------------------------------------

fn tag_for(x: char, y: char) -> FileTag {
    if x == '?' && y == '?' {
        FileTag::Untracked
    } else if x == 'A' || y == 'A' {
        FileTag::New
    } else if x == 'D' || y == 'D' {
        FileTag::Deleted
    } else if x == 'R' {
        FileTag::Renamed
    } else {
        FileTag::Modified
    }
}

pub fn parse_status(out: &str) -> GitStatus {
    let records: Vec<&str> = out.split('\0').filter(|r| !r.is_empty()).collect();
    let mut status = GitStatus::default();
    let mut idx = 0;
    if let Some(first) = records.first() {
        if let Some(rest) = first.strip_prefix("## ") {
            if let Some(name) = rest.strip_prefix("No commits yet on ") {
                status.branch = Some(name.to_string());
            } else {
                let name = rest
                    .split("...")
                    .next()
                    .unwrap_or(rest)
                    .split(' ')
                    .next()
                    .unwrap_or(rest);
                status.branch = Some(name.to_string());
            }
            if let Some(n) = extract_after(rest, "ahead ") {
                status.ahead = n;
            }
            if let Some(n) = extract_after(rest, "behind ") {
                status.behind = n;
            }
            idx = 1;
        }
    }
    while idx < records.len() {
        let rec = records[idx];
        let mut chars = rec.chars();
        let x = chars.next().unwrap_or(' ');
        let y = chars.next().unwrap_or(' ');
        let path = rec.get(3..).unwrap_or("").to_string();
        let mut old_path = None;
        if x == 'R' || x == 'C' {
            idx += 1;
            if idx < records.len() {
                old_path = Some(records[idx].to_string());
            }
        }
        status.files.push(StatusFile {
            path,
            old_path,
            tag: tag_for(x, y),
            staged: x != ' ' && x != '?',
            unstaged: (y != ' ' && y != '?') || x == '?',
        });
        idx += 1;
    }
    status
}

fn extract_after(s: &str, marker: &str) -> Option<u32> {
    let idx = s.find(marker)?;
    let rest = &s[idx + marker.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

pub fn status(repo_root: &Path) -> Result<GitStatus, String> {
    let out = git(repo_root, &["status", "--porcelain=v1", "-z", "-b"])?;
    Ok(parse_status(&out))
}

pub fn stage(repo_root: &Path, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        git(repo_root, &["add", "-A"])?;
    } else {
        let mut args = vec!["add", "--"];
        args.extend(paths.iter().map(|s| s.as_str()));
        git(repo_root, &args)?;
    }
    Ok(())
}

pub fn unstage(repo_root: &Path, paths: &[String]) -> Result<(), String> {
    let mut args = vec!["restore", "--staged", "--"];
    args.extend(paths.iter().map(|s| s.as_str()));
    git(repo_root, &args)?;
    Ok(())
}

pub fn commit(repo_root: &Path, message: &str) -> Result<String, String> {
    git(repo_root, &["commit", "-m", message])?;
    Ok(git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string())
}

pub fn branches(repo_root: &Path) -> Result<BranchInfo, String> {
    let out = git(repo_root, &["branch", "--format=%(refname:short)"])?;
    let branches = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let current_raw = git(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let current = if current_raw == "HEAD" {
        None
    } else {
        Some(current_raw)
    };
    Ok(BranchInfo { current, branches })
}

pub fn switch_branch(repo_root: &Path, name: &str, create: bool) -> Result<(), String> {
    if create {
        git(repo_root, &["switch", "-c", name])?;
    } else {
        git(repo_root, &["switch", name])?;
    }
    Ok(())
}

pub fn create_pr(repo_root: &Path, title: &str, body: &str) -> PrResult {
    let gh_present = Command::new("gh")
        .arg("--version")
        .current_dir(repo_root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !gh_present {
        return PrResult {
            created: false,
            detail: format!("gh pr create --title {title:?} --body {body:?}"),
        };
    }
    match Command::new("gh")
        .args(["pr", "create", "--title", title, "--body", body])
        .current_dir(repo_root)
        .output()
    {
        Ok(output) if output.status.success() => PrResult {
            created: true,
            detail: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        },
        Ok(output) => PrResult {
            created: false,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
        Err(e) => PrResult {
            created: false,
            detail: e.to_string(),
        },
    }
}

// --- diff ----------------------------------------------------------------

pub fn current_branch(repo_root: &Path) -> Option<String> {
    let raw = git(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()?
        .trim()
        .to_string();
    if raw == "HEAD" || raw.is_empty() {
        None
    } else {
        Some(raw)
    }
}

pub fn untracked_files(repo_root: &Path) -> Vec<String> {
    let Ok(out) = git(
        repo_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    ) else {
        return Vec::new();
    };
    out.split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn clean_path(raw: &str) -> Option<String> {
    if raw == "/dev/null" {
        return None;
    }
    Some(
        raw.strip_prefix("a/")
            .or_else(|| raw.strip_prefix("b/"))
            .unwrap_or(raw)
            .to_string(),
    )
}

pub fn untracked_as_diff(repo_root: &Path, rel: &str) -> Option<DiffFile> {
    let content = std::fs::read_to_string(repo_root.join(rel)).ok()?;
    if content.contains(' ') {
        return Some(DiffFile {
            file: rel.to_string(),
            old_path: None,
            tag: DiffTag::New,
            add: 0,
            del: 0,
            rows: vec![DiffRow {
                t: DiffRowKind::Hunk,
                code: "binary file".to_string(),
                ln: None,
            }],
        });
    }
    let stripped = content.strip_suffix('\n').unwrap_or(&content);
    let lines: Vec<&str> = if stripped.is_empty() {
        Vec::new()
    } else {
        stripped.split('\n').collect()
    };
    let mut rows = vec![DiffRow {
        t: DiffRowKind::Hunk,
        code: format!("@@ -0,0 +1,{} @@", lines.len()),
        ln: None,
    }];
    for (i, line) in lines.iter().enumerate() {
        rows.push(DiffRow {
            t: DiffRowKind::Add,
            code: line.to_string(),
            ln: Some((i + 1) as i64),
        });
    }
    Some(DiffFile {
        file: rel.to_string(),
        old_path: None,
        tag: DiffTag::New,
        add: lines.len() as i64,
        del: 0,
        rows,
    })
}

pub fn parse_unified_diff(text: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut cur: Option<DiffFile> = None;
    let mut old_ln: i64 = 0;
    let mut new_ln: i64 = 0;

    macro_rules! flush {
        () => {
            if let Some(f) = cur.take() {
                if !f.file.is_empty() {
                    files.push(f);
                }
            }
        };
    }

    for line in text.lines() {
        if line.starts_with("diff --git ") {
            flush!();
            cur = Some(DiffFile {
                file: String::new(),
                old_path: None,
                tag: DiffTag::Modified,
                add: 0,
                del: 0,
                rows: Vec::new(),
            });
            continue;
        }
        let Some(f) = cur.as_mut() else { continue };
        if line.starts_with("new file") {
            f.tag = DiffTag::New;
        } else if line.starts_with("deleted file") {
            f.tag = DiffTag::Deleted;
        } else if let Some(p) = line.strip_prefix("rename from ") {
            f.old_path = Some(p.to_string());
            f.tag = DiffTag::Renamed;
        } else if let Some(p) = line.strip_prefix("rename to ") {
            f.file = p.to_string();
        } else if let Some(p) = line.strip_prefix("--- ") {
            if let Some(cleaned) = clean_path(p) {
                if f.old_path.is_none() {
                    f.old_path = Some(cleaned);
                }
            }
        } else if let Some(p) = line.strip_prefix("+++ ") {
            match clean_path(p) {
                Some(cleaned) => f.file = cleaned,
                None => f.file = f.old_path.clone().unwrap_or_default(),
            }
        } else if line.starts_with("@@") {
            if let Some((o, n, ctx)) = parse_hunk_header(line) {
                old_ln = o;
                new_ln = n;
                f.rows.push(DiffRow {
                    t: DiffRowKind::Hunk,
                    code: ctx.trim().to_string(),
                    ln: None,
                });
            }
        } else if line.starts_with('\\') {
            // "\ No newline at end of file" — ignored entirely.
        } else if let Some(rest) = line.strip_prefix('+') {
            f.rows.push(DiffRow {
                t: DiffRowKind::Add,
                code: rest.to_string(),
                ln: Some(new_ln),
            });
            new_ln += 1;
            f.add += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            f.rows.push(DiffRow {
                t: DiffRowKind::Del,
                code: rest.to_string(),
                ln: Some(old_ln),
            });
            old_ln += 1;
            f.del += 1;
        } else if let Some(rest) = line.strip_prefix(' ') {
            f.rows.push(DiffRow {
                t: DiffRowKind::Ctx,
                code: rest.to_string(),
                ln: Some(new_ln),
            });
            old_ln += 1;
            new_ln += 1;
        }
    }
    flush!();
    files
}

fn parse_hunk_header(line: &str) -> Option<(i64, i64, String)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_part, rest) = rest.split_once(' ')?;
    let old_start: i64 = old_part.split(',').next()?.parse().ok()?;
    let rest = rest.strip_prefix('+')?;
    let (new_part, rest) = rest.split_once(" @@")?;
    let new_start: i64 = new_part.split(',').next()?.parse().ok()?;
    Some((old_start, new_start, rest.to_string()))
}

pub fn get_diff(repo_root: &Path, rev: Option<&str>) -> Result<DiffSummary, String> {
    let branch = current_branch(repo_root);
    let mut files = if let Some(rev) = rev {
        let out = git(repo_root, &["diff", "--no-color", "-M", rev])?;
        parse_unified_diff(&out)
    } else {
        let out = git(repo_root, &["diff", "--no-color", "-M", "HEAD"])?;
        let mut files = parse_unified_diff(&out);
        for rel in untracked_files(repo_root) {
            if let Some(f) = untracked_as_diff(repo_root, &rel) {
                files.push(f);
            }
        }
        files
    };
    files.sort_by(|a, b| a.file.cmp(&b.file));
    let add = files.iter().map(|f| f.add).sum();
    let del = files.iter().map(|f| f.del).sum();
    Ok(DiffSummary {
        files,
        add,
        del,
        branch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_extracts_branch_ahead_behind_and_file_tags() {
        let out = "## main...origin/main [ahead 1, behind 2]\0M  staged.txt\0 M unstaged.txt\0?? untracked.txt\0";
        let status = parse_status(out);
        assert_eq!(status.branch, Some("main".to_string()));
        assert_eq!(status.ahead, 1);
        assert_eq!(status.behind, 2);
        assert_eq!(status.files.len(), 3);
        assert!(status.files[0].staged && !status.files[0].unstaged);
        assert!(!status.files[1].staged && status.files[1].unstaged);
        assert_eq!(status.files[2].tag, FileTag::Untracked);
    }

    #[test]
    fn parse_status_captures_rename_old_path() {
        let out = "## main\0R  new.txt\0old.txt\0";
        let status = parse_status(out);
        assert_eq!(status.files.len(), 1);
        assert_eq!(status.files[0].tag, FileTag::Renamed);
        assert_eq!(status.files[0].old_path, Some("old.txt".to_string()));
        assert_eq!(status.files[0].path, "new.txt");
    }

    #[test]
    fn parse_status_handles_fresh_repo_with_no_commits() {
        let out = "## No commits yet on main\0?? a.txt\0";
        let status = parse_status(out);
        assert_eq!(status.branch, Some("main".to_string()));
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn parse_unified_diff_modified_file_tracks_add_del_and_line_numbers() {
        let diff = "diff --git a/f.txt b/f.txt\nindex 111..222 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,4 @@\n ctx1\n-old\n+new1\n+new2\n ctx2\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.file, "f.txt");
        assert_eq!(f.tag, DiffTag::Modified);
        assert_eq!(f.add, 2);
        assert_eq!(f.del, 1);
        // ctx1 at new-line 1, old removed at old-line 2, new1 at new-line 2, new2 at new-line 3, ctx2 at new-line 4.
        let lns: Vec<Option<i64>> = f.rows.iter().map(|r| r.ln).collect();
        assert_eq!(lns, vec![None, Some(1), Some(2), Some(2), Some(3), Some(4)]);
    }

    #[test]
    fn parse_unified_diff_new_file_is_all_additions() {
        let diff = "diff --git a/n.txt b/n.txt\nnew file mode 100644\nindex 000..111\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+line1\n+line2\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].tag, DiffTag::New);
        assert_eq!(files[0].add, 2);
        assert_eq!(files[0].del, 0);
        assert_eq!(files[0].file, "n.txt");
    }

    #[test]
    fn parse_unified_diff_deleted_file_named_from_old_side() {
        let diff = "diff --git a/d.txt b/d.txt\ndeleted file mode 100644\nindex 111..000\n--- a/d.txt\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-gone\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].tag, DiffTag::Deleted);
        assert_eq!(files[0].file, "d.txt");
        assert_eq!(files[0].del, 1);
    }

    #[test]
    fn parse_unified_diff_rename_captures_old_and_new_names() {
        let diff = "diff --git a/old.txt b/new.txt\nsimilarity index 100%\nrename from old.txt\nrename to new.txt\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].tag, DiffTag::Renamed);
        assert_eq!(files[0].old_path, Some("old.txt".to_string()));
        assert_eq!(files[0].file, "new.txt");
    }

    #[test]
    fn parse_unified_diff_multiple_files_in_one_blob() {
        let diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-a\n+A\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1,1 +1,1 @@\n-b\n+B\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].file, "a.txt");
        assert_eq!(files[1].file, "b.txt");
    }

    #[test]
    fn parse_unified_diff_empty_diff_is_empty_list() {
        assert_eq!(parse_unified_diff(""), Vec::new());
    }

    #[test]
    fn parse_unified_diff_ignores_no_newline_markers() {
        let diff = "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -1,1 +1,1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";
        let files = parse_unified_diff(diff);
        // hunk + del + add — the two "\ No newline" marker lines contribute no rows.
        assert_eq!(files[0].rows.len(), 3);
        assert_eq!(files[0].add, 1);
        assert_eq!(files[0].del, 1);
    }
}
