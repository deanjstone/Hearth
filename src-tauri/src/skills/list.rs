// Skill discovery for the settings "Skills" panel (Phase 7, tracking issue
// #27). Ported from `electron/main/skills/list.ts`. Pure filesystem
// discovery over `~/.claude/skills(-disabled)/` and
// `<workspace>/.claude/skills(-disabled)/` — no JSON index, the Claude Code
// CLI already treats these directories as its own source of truth.

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub scope: &'static str,
    pub path: String,
    pub enabled: bool,
}

pub fn global_skills_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("skills")
}

/// Extracts `name`/`description` from a `SKILL.md`'s YAML frontmatter block
/// (`---\n...\n---`). Any other frontmatter field is ignored, matching the
/// Electron original's narrow extraction.
fn parse_frontmatter(md: &str) -> (Option<String>, Option<String>) {
    let Some(rest) = md.strip_prefix("---") else {
        return (None, None);
    };
    let rest = rest.trim_start_matches(['\r', '\n']);
    let Some(end) = rest.find("\n---") else {
        return (None, None);
    };
    let block = &rest[..end];
    let field = |key: &str| -> Option<String> {
        block.lines().find_map(|line| {
            let prefix = format!("{key}:");
            line.strip_prefix(&prefix).map(|v| {
                let v = v.trim();
                let stripped = v
                    .strip_prefix(['"', '\''])
                    .and_then(|s| s.strip_suffix(['"', '\'']))
                    .unwrap_or(v);
                stripped.to_string()
            })
        })
    };
    (field("name"), field("description"))
}

fn read_dir_skills(dir: &Path, scope: &'static str, enabled: bool) -> Vec<SkillInfo> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        let Ok(md) = fs::read_to_string(&skill_md) else {
            continue;
        };
        let folder_name = entry.file_name().to_string_lossy().into_owned();
        let (name, description) = parse_frontmatter(&md);
        out.push(SkillInfo {
            name: name.unwrap_or(folder_name),
            description: description.unwrap_or_default(),
            scope,
            path: path.to_string_lossy().into_owned(),
            enabled,
        });
    }
    out
}

fn scope_skills(claude_dir: &Path, scope: &'static str) -> Vec<SkillInfo> {
    let mut out = read_dir_skills(&claude_dir.join("skills"), scope, true);
    out.extend(read_dir_skills(
        &claude_dir.join("skills-disabled"),
        scope,
        false,
    ));
    out
}

pub fn list_skills(workspace_cwd: Option<&Path>) -> Vec<SkillInfo> {
    let home_claude = dirs::home_dir().unwrap_or_default().join(".claude");
    let mut all = scope_skills(&home_claude, "global");
    if let Some(cwd) = workspace_cwd {
        all.extend(scope_skills(&cwd.join(".claude"), "workspace"));
    }
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all
}

pub fn set_skill_enabled(skill_path: &Path, enabled: bool) -> Result<PathBuf, String> {
    let name = skill_path.file_name().ok_or_else(|| {
        format!(
            "refusing to toggle a skill outside a skills folder: {}",
            skill_path.display()
        )
    })?;
    let container = skill_path.parent().ok_or_else(|| {
        format!(
            "refusing to toggle a skill outside a skills folder: {}",
            skill_path.display()
        )
    })?;
    let current_kind = container
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if current_kind != "skills" && current_kind != "skills-disabled" {
        return Err(format!(
            "refusing to toggle a skill outside a skills folder: {}",
            skill_path.display()
        ));
    }
    let claude_dir = container.parent().ok_or_else(|| {
        format!(
            "refusing to toggle a skill outside a skills folder: {}",
            skill_path.display()
        )
    })?;
    let dest_container = claude_dir.join(if enabled { "skills" } else { "skills-disabled" });
    let dest = dest_container.join(name);
    if dest == skill_path {
        return Ok(skill_path.to_path_buf());
    }
    fs::create_dir_all(&dest_container).map_err(|e| e.to_string())?;
    fs::rename(skill_path, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_skill(dir: &Path, name: &str, frontmatter: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), frontmatter).unwrap();
        skill_dir
    }

    #[test]
    fn parses_name_and_description_from_frontmatter() {
        let dir = TempDir::new().unwrap();
        let skills = dir.path().join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_skill(
            &skills,
            "adr",
            "---\nname: adr\ndescription: Write an ADR\n---\nbody",
        );
        let found = read_dir_skills(&skills, "global", true);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "adr");
        assert_eq!(found[0].description, "Write an ADR");
        assert!(found[0].enabled);
    }

    #[test]
    fn falls_back_to_folder_name_when_frontmatter_lacks_name() {
        let dir = TempDir::new().unwrap();
        let skills = dir.path().join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_skill(&skills, "my-skill", "---\ndescription: x\n---\n");
        let found = read_dir_skills(&skills, "global", true);
        assert_eq!(found[0].name, "my-skill");
    }

    #[test]
    fn ignores_folders_without_skill_md() {
        let dir = TempDir::new().unwrap();
        let skills = dir.path().join("skills");
        fs::create_dir_all(skills.join("no-manifest")).unwrap();
        let found = read_dir_skills(&skills, "global", true);
        assert!(found.is_empty());
    }

    #[test]
    fn does_not_throw_on_missing_skills_dir() {
        let dir = TempDir::new().unwrap();
        let found = read_dir_skills(&dir.path().join("nonexistent"), "global", true);
        assert!(found.is_empty());
    }

    #[test]
    fn strips_surrounding_quotes_from_frontmatter_values() {
        let dir = TempDir::new().unwrap();
        let skills = dir.path().join("skills");
        fs::create_dir_all(&skills).unwrap();
        write_skill(
            &skills,
            "quoted",
            "---\nname: \"Quoted Name\"\ndescription: 'Single quoted'\n---\n",
        );
        let found = read_dir_skills(&skills, "global", true);
        assert_eq!(found[0].name, "Quoted Name");
        assert_eq!(found[0].description, "Single quoted");
    }

    #[test]
    fn disabled_skills_report_enabled_false_but_are_still_returned() {
        let dir = TempDir::new().unwrap();
        let disabled = dir.path().join("skills-disabled");
        fs::create_dir_all(&disabled).unwrap();
        write_skill(&disabled, "parked", "---\nname: parked\n---\n");
        let found = read_dir_skills(&disabled, "global", false);
        assert_eq!(found.len(), 1);
        assert!(!found[0].enabled);
    }

    #[test]
    fn set_skill_enabled_round_trips_disable_then_reenable() {
        let dir = TempDir::new().unwrap();
        let claude_dir = dir.path().join(".claude");
        let skills = claude_dir.join("skills");
        let path = write_skill(&skills, "toggleme", "---\nname: toggleme\n---\n");

        let disabled_path = set_skill_enabled(&path, false).unwrap();
        assert!(!path.exists());
        assert!(disabled_path.exists());
        assert_eq!(
            disabled_path,
            claude_dir.join("skills-disabled").join("toggleme")
        );

        let reenabled_path = set_skill_enabled(&disabled_path, true).unwrap();
        assert_eq!(reenabled_path, path);
        assert!(path.exists());
        assert!(!disabled_path.exists());
    }

    #[test]
    fn set_skill_enabled_no_op_when_already_in_desired_state() {
        let dir = TempDir::new().unwrap();
        let skills = dir.path().join(".claude").join("skills");
        let path = write_skill(&skills, "stable", "---\nname: stable\n---\n");
        let result = set_skill_enabled(&path, true).unwrap();
        assert_eq!(result, path);
        assert!(path.exists());
    }

    #[test]
    fn set_skill_enabled_refuses_a_path_outside_a_skills_folder() {
        let dir = TempDir::new().unwrap();
        let stray = dir.path().join("random").join("thing");
        fs::create_dir_all(&stray).unwrap();
        let err = set_skill_enabled(&stray, false).unwrap_err();
        assert!(err.starts_with("refusing to toggle a skill outside a skills folder"));
    }
}
