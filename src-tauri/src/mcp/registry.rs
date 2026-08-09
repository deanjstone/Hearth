// Ported from electron/main/mcp/registry.ts (Phase 5, tracking issue #27).
// User-configured MCP servers. Persisted as plain JSON under the app's data
// dir; the secret values these servers need are referenced here by key only
// (never copied in) — secrets storage itself is out of scope for this MVP
// (spec #26's Out of Scope), same standing gap Phase 3's auth path already
// left (see agent_commands.rs's own comment on the deferred secrets-backed
// path). The registry is merged into every new ACP session alongside the
// built-in `hearth` bridge — see `to_acp.rs`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// One env var (stdio) or header (http/sse) the server needs. Either a
/// literal value or a reference into the secret store — never both
/// meaningfully set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpEnvVar {
    pub name: String,
    /// Resolve from the secret store at session-creation time.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub secret_key: Option<String>,
    /// A literal value (stored in this config file as-is).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpTransport {
    Stdio { command: String, args: Vec<String> },
    Http { url: String },
    Sse { url: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub transport: McpTransport,
    /// Env vars (stdio) or HTTP headers (http/sse).
    pub env: Vec<McpEnvVar>,
}

/// A new server before it gets an id — the Rust shape of TS's
/// `Omit<McpServerConfig, 'id'>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInput {
    pub name: String,
    pub enabled: bool,
    pub transport: McpTransport,
    pub env: Vec<McpEnvVar>,
}

/// Updates arrive as partials — the Rust shape of TS's
/// `Partial<McpServerInput>`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerPatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub transport: Option<McpTransport>,
    pub env: Option<Vec<McpEnvVar>>,
}

/// Monotonic id: `mcp_<millis-since-epoch base36>_<counter>`. Hand-rolled
/// rather than pulling in the `uuid` crate, mirroring `sessions/store.rs`'s
/// own `s_<base36 timestamp>_<counter>` id scheme (same minimal-dependency
/// call already made there).
fn to_base36(mut n: u128) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

fn new_id(counter: &AtomicU64) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "mcp_{}_{}",
        to_base36(millis),
        counter.fetch_add(1, Ordering::SeqCst)
    )
}

pub struct McpRegistry {
    file_path: PathBuf,
    servers: Mutex<Vec<McpServerConfig>>,
    counter: AtomicU64,
}

impl McpRegistry {
    pub fn new(file_path: PathBuf) -> Self {
        let servers = Self::load(&file_path);
        Self {
            file_path,
            servers: Mutex::new(servers),
            counter: AtomicU64::new(0),
        }
    }

    pub fn list(&self) -> Vec<McpServerConfig> {
        self.servers.lock().unwrap().clone()
    }

    pub fn add(&self, input: McpServerInput) -> Result<McpServerConfig, String> {
        let server = McpServerConfig {
            id: new_id(&self.counter),
            name: input.name,
            enabled: input.enabled,
            transport: input.transport,
            env: input.env,
        };
        let mut servers = self.servers.lock().unwrap();
        servers.push(server.clone());
        self.persist(&servers)?;
        Ok(server)
    }

    pub fn update(
        &self,
        id: &str,
        patch: McpServerPatch,
    ) -> Result<Option<McpServerConfig>, String> {
        let mut servers = self.servers.lock().unwrap();
        let Some(s) = servers.iter_mut().find(|s| s.id == id) else {
            return Ok(None);
        };
        if let Some(name) = patch.name {
            s.name = name;
        }
        if let Some(enabled) = patch.enabled {
            s.enabled = enabled;
        }
        if let Some(transport) = patch.transport {
            s.transport = transport;
        }
        if let Some(env) = patch.env {
            s.env = env;
        }
        let updated = s.clone();
        self.persist(&servers)?;
        Ok(Some(updated))
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let mut servers = self.servers.lock().unwrap();
        if let Some(s) = servers.iter_mut().find(|s| s.id == id) {
            if s.enabled != enabled {
                s.enabled = enabled;
                self.persist(&servers)?;
            }
        }
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut servers = self.servers.lock().unwrap();
        let before = servers.len();
        servers.retain(|s| s.id != id);
        if servers.len() != before {
            self.persist(&servers)?;
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<McpServerConfig> {
        self.servers
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// A file that isn't valid JSON, or isn't a JSON array, loads as empty —
    /// matching `registry.ts`'s `if (Array.isArray(parsed))` guard. Unlike
    /// the TS original (untyped, so a malformed element just gets carried
    /// along until something downstream chokes on it), Rust's typed
    /// `Vec<McpServerConfig>` deserialization fails the WHOLE array if any
    /// one element doesn't match the shape — parsing element-by-element and
    /// dropping only the bad ones avoids a single corrupt entry silently
    /// wiping every other configured server on next load.
    fn load(file_path: &PathBuf) -> Vec<McpServerConfig> {
        let Ok(text) = fs::read_to_string(file_path) else {
            return Vec::new();
        };
        let Ok(raw) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
            return Vec::new();
        };
        raw.into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect()
    }

    /// Write-through-temp-then-rename: a concurrent reader always sees either
    /// the old or the new file whole, never a partial write — same pattern
    /// `sessions/store.rs`'s `write_index` already established.
    fn persist(&self, servers: &[McpServerConfig]) -> Result<(), String> {
        let dir = self
            .file_path
            .parent()
            .ok_or("mcp registry path has no parent directory")?;
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(servers).map_err(|e| e.to_string())?;
        // Append ".tmp" to the whole filename rather than `with_extension`,
        // which replaces (not appends to) the extension after the last dot.
        let mut tmp = self.file_path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        fs::write(&tmp, json).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.file_path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio_input(name: &str) -> McpServerInput {
        McpServerInput {
            name: name.to_string(),
            enabled: true,
            transport: McpTransport::Stdio {
                command: "my-server".to_string(),
                args: vec!["--flag".to_string()],
            },
            env: vec![],
        }
    }

    fn tmp_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-servers.json");
        (dir, path)
    }

    #[test]
    fn add_assigns_an_id_and_persists() {
        let (_dir, file) = tmp_file();
        let reg = McpRegistry::new(file.clone());
        let s = reg.add(stdio_input("Foo")).unwrap();
        assert!(!s.id.is_empty());
        assert_eq!(McpRegistry::new(file).list().len(), 1);
    }

    #[test]
    fn update_patches_fields_but_keeps_id() {
        let (_dir, file) = tmp_file();
        let reg = McpRegistry::new(file);
        let s = reg.add(stdio_input("Foo")).unwrap();
        let updated = reg
            .update(
                &s.id,
                McpServerPatch {
                    name: Some("Bar".to_string()),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(updated.id, s.id);
        assert_eq!(updated.name, "Bar");
    }

    #[test]
    fn update_missing_id_returns_none() {
        let (_dir, file) = tmp_file();
        let reg = McpRegistry::new(file);
        assert!(reg
            .update("nope", McpServerPatch::default())
            .unwrap()
            .is_none());
    }

    #[test]
    fn set_enabled_and_remove() {
        let (_dir, file) = tmp_file();
        let reg = McpRegistry::new(file);
        let s = reg.add(stdio_input("Foo")).unwrap();
        reg.set_enabled(&s.id, false).unwrap();
        assert!(!reg.get(&s.id).unwrap().enabled);
        reg.remove(&s.id).unwrap();
        assert_eq!(reg.list().len(), 0);
    }

    #[test]
    fn add_generates_distinct_ids_for_rapid_calls() {
        let (_dir, file) = tmp_file();
        let reg = McpRegistry::new(file);
        let a = reg.add(stdio_input("A")).unwrap();
        let b = reg.add(stdio_input("B")).unwrap();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn env_secret_key_round_trips_through_persistence() {
        let (_dir, file) = tmp_file();
        let reg = McpRegistry::new(file.clone());
        let mut input = stdio_input("Foo");
        input.env = vec![McpEnvVar {
            name: "TOKEN".to_string(),
            secret_key: Some("mcp.foo.TOKEN".to_string()),
            value: None,
        }];
        let s = reg.add(input).unwrap();
        let reloaded = McpRegistry::new(file).get(&s.id).unwrap();
        assert_eq!(reloaded.env[0].secret_key.as_deref(), Some("mcp.foo.TOKEN"));
    }

    #[test]
    fn add_reports_an_error_when_the_write_fails() {
        // Point the registry file inside a path component that's actually a
        // file, not a directory — `create_dir_all` on its parent fails, so
        // `persist` can't succeed. Mirrors the ipc.ts contract: a disk error
        // surfaces to the caller instead of silently reporting success.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("not-a-directory");
        fs::write(&blocker, "x").unwrap();
        let reg = McpRegistry::new(blocker.join("mcp-servers.json"));
        assert!(reg.add(stdio_input("Foo")).is_err());
    }

    #[test]
    fn load_drops_only_the_malformed_entry_not_the_whole_list() {
        let (_dir, file) = tmp_file();
        fs::write(
            &file,
            serde_json::json!([
                { "id": "1", "name": "Good", "enabled": true, "transport": { "type": "stdio", "command": "foo", "args": [] }, "env": [] },
                { "id": "2", "name": "Bad", "enabled": true, "transport": { "type": "not-a-real-transport" }, "env": [] },
            ])
            .to_string(),
        )
        .unwrap();
        let reg = McpRegistry::new(file);
        let list = reg.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Good");
    }
}
