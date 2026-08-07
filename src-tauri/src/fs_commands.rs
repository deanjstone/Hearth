// The `window.hearth.files` Tauri surface (Phase 7, tracking issue #27).
// Ported from `electron/main/fs/files.ts`'s `listDir`/`readFile`/`writeFileGuarded`.
//
// Only `state.repo_root` is supported as a workspace root — the Electron
// original resolves `cwd` against a full multi-workspace registry via its
// `at(cwd)` helper, but that registry isn't ported (see
// `workspaces_commands.rs`'s own header comment on why it's a deliberate
// stub); every call here ignores a passed `cwd` and operates on the repo
// root, which is the only workspace this port's `AppState` actually has.

use crate::selfmod::scope_guard::{classify_write, ScopeTier};
use crate::selfmod_commands::AppState;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const IGNORED: &[&str] = &[
    ".git",
    "node_modules",
    "out",
    "dist",
    ".DS_Store",
    ".hearth",
];
const MAX_READ: u64 = 2 * 1024 * 1024;
const PROTECTED_WRITE_MESSAGE: &str = "protected path — not editable";

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub rel: String,
    pub dir: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    pub rel: String,
    pub content: String,
    pub readonly: bool,
}

/// Mirrors `safeJoin`: resolves `rel` against `root` and rejects any result
/// that escapes it (via `..` or an absolute `rel`), the only traversal guard
/// the Electron original has.
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let abs = root.join(rel);
    let rel_back = pathdiff(&abs, root);
    if rel_back.starts_with("..") || Path::new(&rel_back).is_absolute() {
        return Err("path escapes workspace".to_string());
    }
    Ok(abs)
}

/// Lexical (non-canonicalizing) relative-path diff, matching Node's
/// `path.relative` closely enough for the traversal check above — neither
/// side touches the filesystem or resolves symlinks.
fn pathdiff(target: &Path, base: &Path) -> String {
    let target_norm: Vec<_> = target.components().collect();
    let base_norm: Vec<_> = base.components().collect();
    let common = target_norm
        .iter()
        .zip(base_norm.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..base_norm.len() {
        parts.push("..".to_string());
    }
    for c in &target_norm[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

pub fn list_dir(root: &Path, rel: &str) -> Result<Vec<FileEntry>, String> {
    let abs = if rel.is_empty() {
        root.to_path_buf()
    } else {
        safe_join(root, rel)?
    };
    let mut entries = Vec::new();
    let read_dir = match fs::read_dir(&abs) {
        Ok(rd) => rd,
        Err(e) => return Err(e.to_string()),
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if IGNORED.contains(&name.as_str()) {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let entry_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        entries.push(FileEntry {
            name,
            rel: entry_rel,
            dir: is_dir,
        });
    }
    entries.sort_by(|a, b| {
        if a.dir == b.dir {
            a.name.cmp(&b.name)
        } else if a.dir {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    });
    Ok(entries)
}

pub fn read_file(root: &Path, rel: &str) -> Result<FileContent, String> {
    let abs = safe_join(root, rel)?;
    let meta = fs::metadata(&abs).map_err(|e| e.to_string())?;
    if meta.len() > MAX_READ {
        let kb = (meta.len() as f64 / 1024.0).round() as u64;
        return Ok(FileContent {
            rel: rel.to_string(),
            content: format!("// {rel} is {kb} KB — too large to edit here."),
            readonly: true,
        });
    }
    let bytes = fs::read(&abs).map_err(|e| e.to_string())?;
    if bytes.contains(&0) {
        return Ok(FileContent {
            rel: rel.to_string(),
            content: format!("// {rel} is a binary file."),
            readonly: true,
        });
    }
    Ok(FileContent {
        rel: rel.to_string(),
        content: String::from_utf8_lossy(&bytes).into_owned(),
        readonly: false,
    })
}

fn write_file(root: &Path, rel: &str, content: &str) -> Result<(), String> {
    let abs = safe_join(root, rel)?;
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&abs, content).map_err(|e| e.to_string())
}

/// Mirrors `writeFileGuarded`: the scope guard only runs when `root` is
/// (case-foldedly) the repo root — a different workspace root has no
/// canvas/protected/blocked tiering at all, subject only to `safe_join`'s
/// traversal guard.
pub fn write_file_guarded(
    root: &Path,
    repo_root: &Path,
    rel: &str,
    content: &str,
) -> Result<(), String> {
    let same_root =
        root.to_string_lossy().to_lowercase() == repo_root.to_string_lossy().to_lowercase();
    if same_root {
        let decision = classify_write(rel, repo_root);
        if decision.tier != ScopeTier::Canvas {
            let reason = decision
                .reason
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            return Err(format!("{PROTECTED_WRITE_MESSAGE}: {rel}{reason}"));
        }
    }
    write_file(root, rel, content)
}

#[tauri::command]
pub fn fs_list(
    state: tauri::State<AppState>,
    cwd: Option<String>,
    rel: Option<String>,
) -> Result<Vec<FileEntry>, String> {
    let _ = cwd;
    list_dir(&state.repo_root, rel.as_deref().unwrap_or(""))
}

#[tauri::command]
pub fn fs_read(
    state: tauri::State<AppState>,
    cwd: Option<String>,
    rel: String,
) -> Result<FileContent, String> {
    let _ = cwd;
    read_file(&state.repo_root, &rel)
}

#[tauri::command]
pub fn fs_write(
    state: tauri::State<AppState>,
    cwd: Option<String>,
    rel: String,
    content: String,
) -> Result<(), String> {
    let _ = cwd;
    write_file_guarded(&state.repo_root, &state.repo_root, &rel, &content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_file_creates_missing_parent_dirs_and_round_trips() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), ".hearth/scratchpad.md", "hello").unwrap();
        let content = read_file(dir.path(), ".hearth/scratchpad.md").unwrap();
        assert_eq!(content.content, "hello");
        assert!(!content.readonly);
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let dir = TempDir::new().unwrap();
        let err = write_file(dir.path(), "../escape.md", "x").unwrap_err();
        assert_eq!(err, "path escapes workspace");
    }

    #[test]
    fn list_dir_omits_dot_hearth_but_lists_siblings() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".hearth")).unwrap();
        fs::write(dir.path().join("visible.md"), "x").unwrap();
        let entries = list_dir(dir.path(), "").unwrap();
        assert!(entries.iter().any(|e| e.name == "visible.md"));
        assert!(!entries.iter().any(|e| e.name == ".hearth"));
    }

    #[test]
    fn list_dir_sorts_dirs_first_then_alpha() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("b.txt"), "x").unwrap();
        fs::write(dir.path().join("a.txt"), "x").unwrap();
        fs::create_dir(dir.path().join("z-dir")).unwrap();
        let entries = list_dir(dir.path(), "").unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["z-dir", "a.txt", "b.txt"]);
    }

    #[test]
    fn read_file_reports_binary_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("bin.dat"), [0u8, 1, 2]).unwrap();
        let content = read_file(dir.path(), "bin.dat").unwrap();
        assert!(content.readonly);
        assert_eq!(content.content, "// bin.dat is a binary file.");
    }

    #[test]
    fn write_file_guarded_denies_protected_island_write() {
        let dir = TempDir::new().unwrap();
        let err = write_file_guarded(
            dir.path(),
            dir.path(),
            "src-tauri/src/selfmod/boot_watchdog.rs",
            "x",
        )
        .unwrap_err();
        assert!(err.starts_with(PROTECTED_WRITE_MESSAGE));
    }

    #[test]
    fn write_file_guarded_allows_canvas_write() {
        let dir = TempDir::new().unwrap();
        write_file_guarded(dir.path(), dir.path(), "src/app/chat/ChatView.tsx", "x").unwrap();
        assert!(dir.path().join("src/app/chat/ChatView.tsx").exists());
    }

    #[test]
    fn write_file_guarded_skips_tiering_for_a_different_workspace_root() {
        let repo = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        // Same island-shaped relative path is allowed when root != repo_root.
        write_file_guarded(
            other.path(),
            repo.path(),
            "src-tauri/src/selfmod/boot_watchdog.rs",
            "x",
        )
        .unwrap();
        assert!(other
            .path()
            .join("src-tauri/src/selfmod/boot_watchdog.rs")
            .exists());
    }
}
