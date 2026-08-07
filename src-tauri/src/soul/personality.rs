// Personality/memory configuration for the settings "Personality" panel and
// "Memory" viewer (Phase 7, tracking issue #27). Ported from
// `electron/main/soul/soul.ts`. Writes an idempotent managed block (see
// `managed_block.rs`) into both backends' global instruction files
// (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`) on every personality/memory
// change — the Electron original always fans out to both regardless of
// which backend is currently active.

use super::managed_block::upsert_block;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SoulConfig {
    pub length: Length,
    pub directness: Directness,
    pub density: Density,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Length {
    Short,
    Balanced,
    Thorough,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Directness {
    Gentle,
    Direct,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Density {
    Compact,
    Roomy,
}

pub const DEFAULT_SOUL: SoulConfig = SoulConfig {
    length: Length::Balanced,
    directness: Directness::Direct,
    density: Density::Compact,
};

const OPERATING: &str = "The user may keep a scratchpad at .hearth/scratchpad.md — read it for context, but never write to it.";

pub fn compile_soul(c: &SoulConfig) -> String {
    let length_line = match c.length {
        Length::Short => "Keep replies short and to the point; lead with the answer.",
        Length::Balanced => "Aim for balanced replies — enough to be useful, no filler.",
        Length::Thorough => "Be thorough; cover edge cases and reasoning when it helps.",
    };
    let directness_line = match c.directness {
        Directness::Gentle => "Use a warm, encouraging tone.",
        Directness::Direct => "Be direct and plainspoken; say when something is wrong and why.",
    };
    let density_line = match c.density {
        Density::Compact => "Prefer compact formatting; use lists sparingly.",
        Density::Roomy => "Use generous structure — headings and lists — when it aids scanning.",
    };
    format!("## Soul\n\n{length_line}\n{directness_line}\n{density_line}")
}

pub const BACKENDS: &[&str] = &["claude", "codex"];

pub fn global_instructions_path_in(home: &std::path::Path, backend: &str) -> PathBuf {
    match backend {
        "codex" => home.join(".codex").join("AGENTS.md"),
        _ => home.join(".claude").join("CLAUDE.md"),
    }
}

/// Takes an explicit home directory rather than reading `$HOME`/`dirs::home_dir()`
/// internally, so tests can point at a tempdir without mutating global process
/// env state (which would race across parallel test threads).
pub struct SoulService {
    home: PathBuf,
    backends: &'static [&'static str],
}

impl SoulService {
    pub fn new() -> Self {
        Self {
            home: dirs::home_dir().unwrap_or_default(),
            backends: BACKENDS,
        }
    }

    #[cfg(test)]
    fn with_home(home: PathBuf) -> Self {
        Self {
            home,
            backends: BACKENDS,
        }
    }

    fn write_managed(
        &self,
        backend: &str,
        soul: Option<&str>,
        memory: Option<&str>,
    ) -> Result<(), String> {
        let path = global_instructions_path_in(&self.home, backend);
        let current = fs::read_to_string(&path).unwrap_or_default();
        let mut content = current;
        if let Some(soul) = soul {
            let managed_body = format!("{OPERATING}\n\n{soul}");
            content = upsert_block(&content, "managed", &managed_body);
        }
        if let Some(memory) = memory {
            let trimmed = memory.trim();
            let body = if trimmed.is_empty() {
                String::new()
            } else {
                format!("## Memory\n\n{trimmed}")
            };
            content = upsert_block(&content, "memory", &body);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&path, content).map_err(|e| e.to_string())
    }

    pub fn set_personality(&self, config: &SoulConfig) -> Result<(), String> {
        let soul = compile_soul(config);
        for backend in self.backends {
            self.write_managed(backend, Some(&soul), None)?;
        }
        Ok(())
    }

    /// Defaults to the `claude` backend's file — asymmetric with
    /// `set_memory`, which writes both, matching the Electron original.
    pub fn get_memory(&self, backend: &str) -> String {
        let path = global_instructions_path_in(&self.home, backend);
        let content = fs::read_to_string(&path).unwrap_or_default();
        let Some(block) = super::managed_block::read_block(&content, "memory") else {
            return String::new();
        };
        block
            .strip_prefix("## Memory")
            .map(|s| s.trim().to_string())
            .unwrap_or(block)
    }

    pub fn set_memory(&self, memory: &str) -> Result<(), String> {
        for backend in self.backends {
            self.write_managed(backend, None, Some(memory))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_soul_contains_expected_markers() {
        let text = compile_soul(&DEFAULT_SOUL);
        assert!(text.contains("## Soul"));
        assert!(text.to_lowercase().contains("direct"));
    }

    #[test]
    fn set_personality_writes_both_backend_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = SoulService::with_home(dir.path().to_path_buf());
        service.set_personality(&DEFAULT_SOUL).unwrap();
        let claude = fs::read_to_string(dir.path().join(".claude/CLAUDE.md")).unwrap();
        let codex = fs::read_to_string(dir.path().join(".codex/AGENTS.md")).unwrap();
        assert!(claude.contains("## Soul"));
        assert!(codex.contains("## Soul"));
    }

    #[test]
    fn set_memory_then_get_memory_round_trips_on_claude_backend() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = SoulService::with_home(dir.path().to_path_buf());
        service.set_memory("remember this").unwrap();
        assert_eq!(service.get_memory("claude"), "remember this");
    }

    #[test]
    fn set_memory_empty_clears_the_block() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = SoulService::with_home(dir.path().to_path_buf());
        service.set_memory("something").unwrap();
        service.set_memory("").unwrap();
        assert_eq!(service.get_memory("claude"), "");
    }

    #[test]
    fn set_personality_preserves_a_separately_set_memory_block() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = SoulService::with_home(dir.path().to_path_buf());
        service.set_memory("keep me").unwrap();
        service.set_personality(&DEFAULT_SOUL).unwrap();
        assert_eq!(service.get_memory("claude"), "keep me");
    }
}
