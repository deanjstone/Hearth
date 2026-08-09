// Ported from electron/main/micro-apps/scaffold.ts (Phase 6, tracking issue
// #27). Scaffolds a standalone micro-app from templates/micro-app into
// micro-apps/<name>. A micro-app is its own Vite + React project with its
// own deps — isolated from Hearth, embedded later via iframe (behind
// csp_proxy.rs).
//
// Optionally starts from a *starter*: a one-file variant under
// templates/starters/<id> that overlays the base template's src/App.tsx (so
// starters carry no boilerplate).

use crate::micro_apps::validate::assert_app_name;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const PLACEHOLDER_FILES: [&str; 3] = ["package.json", "index.html", "src/App.tsx"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScaffoldResult {
    pub name: String,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct StarterInfo {
    pub id: String,
    pub title: String,
    pub description: String,
}

fn starters_dir(repo_root: &Path) -> PathBuf {
    repo_root.join("templates").join("starters")
}

/// The blank starter every gallery shows first — maps to the base template.
pub fn blank_starter() -> StarterInfo {
    StarterInfo {
        id: String::new(),
        title: "Blank".to_string(),
        description: "An empty micro-app to build from scratch.".to_string(),
    }
}

#[derive(Default, serde::Deserialize)]
struct RawStarterMeta {
    title: Option<String>,
    description: Option<String>,
}

fn read_starter_meta(dir: &Path) -> RawStarterMeta {
    let Ok(text) = fs::read_to_string(dir.join("starter.json")) else {
        return RawStarterMeta::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// List the available starters (blank first, then templates/starters/<id>).
pub fn list_starters(repo_root: &Path) -> Vec<StarterInfo> {
    let dir = starters_dir(repo_root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return vec![blank_starter()];
    };
    let mut found: Vec<StarterInfo> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir() && e.path().join("App.tsx").exists())
        .map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            let meta = read_starter_meta(&e.path());
            let title = meta
                .title
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| id.clone());
            StarterInfo {
                id,
                title,
                description: meta.description.unwrap_or_default(),
            }
        })
        .collect();
    found.sort_by(|a, b| a.title.cmp(&b.title));
    let mut out = vec![blank_starter()];
    out.extend(found);
    out
}

/// Recursive directory copy — Rust's std has no `cpSync`-equivalent.
fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).map_err(|e| e.to_string())?;
        }
        // Symlinks in a template are unexpected; skip rather than follow.
    }
    Ok(())
}

pub fn scaffold_micro_app(
    repo_root: &Path,
    raw_name: &str,
    starter: Option<&str>,
) -> Result<ScaffoldResult, String> {
    let name = assert_app_name(raw_name)?;

    let template_dir = repo_root.join("templates").join("micro-app");
    let apps_dir = repo_root.join("micro-apps");
    fs::create_dir_all(&apps_dir).map_err(|e| e.to_string())?;

    let dest = apps_dir.join(&name);
    if dest.exists() {
        return Err(format!("Already exists: {}", dest.display()));
    }
    if !template_dir.exists() {
        return Err(format!("Template missing: {}", template_dir.display()));
    }

    copy_dir_all(&template_dir, &dest)?;

    for file in PLACEHOLDER_FILES {
        let path = dest.join(file);
        if !path.exists() {
            continue;
        }
        let contents = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        fs::write(&path, contents.replace("{{name}}", &name)).map_err(|e| e.to_string())?;
    }

    // Overlay a starter's App.tsx, if one was chosen. Validated by
    // membership in the discovered list (not the raw string) so a bogus id
    // can't escape the starters dir.
    if let Some(starter) = starter {
        let known = list_starters(repo_root).iter().any(|s| s.id == starter);
        if !known {
            return Err(format!("Unknown starter: {starter}"));
        }
        let src = starters_dir(repo_root).join(starter).join("App.tsx");
        let contents = fs::read_to_string(&src).map_err(|e| e.to_string())?;
        fs::write(dest.join("src").join("App.tsx"), contents).map_err(|e| e.to_string())?;
    }

    Ok(ScaffoldResult { name, dir: dest })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_template(repo_root: &Path) {
        let template_dir = repo_root.join("templates").join("micro-app");
        fs::create_dir_all(template_dir.join("src")).unwrap();
        fs::write(template_dir.join("package.json"), r#"{"name":"{{name}}"}"#).unwrap();
        fs::write(template_dir.join("index.html"), "<title>{{name}}</title>").unwrap();
        fs::write(
            template_dir.join("src").join("App.tsx"),
            "export default function {{name}}() {}",
        )
        .unwrap();
    }

    #[test]
    fn scaffold_copies_the_template_and_replaces_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        make_template(dir.path());
        let result = scaffold_micro_app(dir.path(), "my-app", None).unwrap();
        assert_eq!(result.name, "my-app");
        let pkg = fs::read_to_string(result.dir.join("package.json")).unwrap();
        assert_eq!(pkg, r#"{"name":"my-app"}"#);
        let app = fs::read_to_string(result.dir.join("src").join("App.tsx")).unwrap();
        assert_eq!(app, "export default function my-app() {}");
    }

    #[test]
    fn scaffold_rejects_an_invalid_name() {
        let dir = tempfile::tempdir().unwrap();
        make_template(dir.path());
        assert!(scaffold_micro_app(dir.path(), "../../etc", None).is_err());
    }

    #[test]
    fn scaffold_rejects_an_already_existing_app() {
        let dir = tempfile::tempdir().unwrap();
        make_template(dir.path());
        scaffold_micro_app(dir.path(), "my-app", None).unwrap();
        assert!(scaffold_micro_app(dir.path(), "my-app", None).is_err());
    }

    #[test]
    fn scaffold_rejects_a_missing_template() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scaffold_micro_app(dir.path(), "my-app", None).is_err());
    }

    #[test]
    fn scaffold_rejects_an_unknown_starter() {
        let dir = tempfile::tempdir().unwrap();
        make_template(dir.path());
        assert!(scaffold_micro_app(dir.path(), "my-app", Some("nope")).is_err());
    }

    #[test]
    fn scaffold_overlays_a_known_starter() {
        let dir = tempfile::tempdir().unwrap();
        make_template(dir.path());
        let starter_dir = dir.path().join("templates").join("starters").join("todo");
        fs::create_dir_all(&starter_dir).unwrap();
        fs::write(
            starter_dir.join("App.tsx"),
            "export default function Todo() {}",
        )
        .unwrap();

        let result = scaffold_micro_app(dir.path(), "my-app", Some("todo")).unwrap();
        let app = fs::read_to_string(result.dir.join("src").join("App.tsx")).unwrap();
        assert_eq!(app, "export default function Todo() {}");
    }

    #[test]
    fn list_starters_always_includes_blank_first() {
        let dir = tempfile::tempdir().unwrap();
        let starters = list_starters(dir.path());
        assert_eq!(starters.len(), 1);
        assert_eq!(starters[0].id, "");
        assert_eq!(starters[0].title, "Blank");
    }

    #[test]
    fn list_starters_finds_a_starter_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let starter_dir = dir.path().join("templates").join("starters").join("todo");
        fs::create_dir_all(&starter_dir).unwrap();
        fs::write(
            starter_dir.join("App.tsx"),
            "export default function Todo() {}",
        )
        .unwrap();
        fs::write(
            starter_dir.join("starter.json"),
            serde_json::json!({ "title": "Todo List", "description": "A simple todo app" })
                .to_string(),
        )
        .unwrap();

        let starters = list_starters(dir.path());
        assert_eq!(starters.len(), 2);
        assert_eq!(starters[1].id, "todo");
        assert_eq!(starters[1].title, "Todo List");
        assert_eq!(starters[1].description, "A simple todo app");
    }

    #[test]
    fn list_starters_ignores_a_dir_without_app_tsx() {
        let dir = tempfile::tempdir().unwrap();
        let bogus_dir = dir
            .path()
            .join("templates")
            .join("starters")
            .join("not-a-starter");
        fs::create_dir_all(&bogus_dir).unwrap();
        let starters = list_starters(dir.path());
        assert_eq!(starters.len(), 1);
    }
}
