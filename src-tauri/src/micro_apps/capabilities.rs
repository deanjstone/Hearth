// Ported from electron/main/micro-apps/capabilities.ts (Phase 6, tracking
// issue #27). Per-app egress capability grants (W6 of the sandbox-hardening
// plan).
//
// A micro-app's code is agent-authored and untrusted. By default it can reach
// nothing external (the per-app CSP floor in csp_proxy.rs). To connect a
// micro-app to a real service, the agent *requests* hosts in a manifest
// (micro-apps/<name>/hearth.app.json); the USER approves them; and only the
// approved set — stored here, never the agent-editable manifest — widens
// that one app's connect-src. A hostile app cannot self-grant: editing its
// manifest just moves a host back to "pending" until the user approves it
// again.

use crate::micro_apps::validate::assert_app_name;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostRequest {
    /// A normalized https origin, e.g. "https://www.googleapis.com".
    pub host: String,
    /// Human-readable justification shown in the approval prompt.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppCapabilities {
    /// Hosts the user has approved for this app — the source of truth for egress.
    pub approved: Vec<String>,
    /// Requested-but-not-yet-approved hosts (manifest minus approved).
    pub pending: Vec<HostRequest>,
}

/// Hosts we never allow regardless of approval: loopback / unspecified /
/// link-local, which would let a frame reach Hearth's own broker or other
/// local services and turn an egress grant into local SSRF.
fn is_blocked_hostname(host: &str) -> bool {
    matches!(
        host,
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "[::1]"
    )
}

/// A real, public-looking dotted DNS name: two or more labels, each
/// alphanumeric with internal hyphens. Rejects wildcards, bare single
/// labels, and IP literals. Hand-checked rather than pulling in `regex` for
/// one anchored pattern — mirrors validate.rs's own call.
fn is_valid_hostname(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels.iter().all(|label| is_valid_label(label))
}

fn is_valid_label(label: &str) -> bool {
    let chars: Vec<char> = label.chars().collect();
    if chars.is_empty() {
        return false;
    }
    let is_alnum = |c: &char| c.is_ascii_lowercase() || c.is_ascii_digit();
    if !is_alnum(&chars[0]) || !is_alnum(chars.last().unwrap()) {
        return false;
    }
    if chars.len() <= 2 {
        // Single- or two-char labels are fully covered by the first/last
        // check above — no middle slice to validate (and `chars.len() - 1`
        // would underflow/mis-slice for len 1).
        return true;
    }
    chars[1..chars.len() - 1]
        .iter()
        .all(|c| is_alnum(c) || *c == '-')
}

/// Normalize a requested host to an exact https origin, or `None` if it
/// isn't one we permit. Only https, only an origin (scheme+host+optional
/// port) — no paths, no wildcards, no embedded credentials, no loopback.
/// Pure + unit-tested.
pub fn normalize_host(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let url = url::Url::parse(trimmed).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let path = url.path();
    if path != "/" && !path.is_empty() {
        return None;
    }
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if is_blocked_hostname(&host) {
        return None;
    }
    if !is_valid_hostname(&host) {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

#[derive(Deserialize, Default)]
struct RawHostEntry {
    host: Option<String>,
    reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawManifest {
    hosts: Option<Vec<RawHostEntry>>,
}

/// Read + validate a micro-app's requested hosts from its manifest. Unknown,
/// malformed, or disallowed entries are dropped (never fails — a bad
/// manifest just yields fewer requests). Pure given the filesystem.
pub fn read_manifest(repo_root: &Path, name: &str) -> Vec<HostRequest> {
    let Ok(name) = assert_app_name(name) else {
        return Vec::new();
    };
    let path = repo_root
        .join("micro-apps")
        .join(&name)
        .join("hearth.app.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<RawManifest>(&text) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for entry in parsed.hosts.unwrap_or_default() {
        let Some(host) = entry.host.as_deref().and_then(normalize_host) else {
            continue;
        };
        if !seen.insert(host.clone()) {
            continue;
        }
        out.push(HostRequest {
            host,
            reason: entry.reason.unwrap_or_default(),
        });
    }
    out
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoreEntry {
    approved: Vec<String>,
}

type StoreShape = BTreeMap<String, StoreEntry>;

/// The approved-host store. Persisted under the app data dir as plain JSON
/// (the approved host list is not a secret — the secrets themselves live in
/// a secret store, out of scope for this MVP per spec #26, same standing gap
/// mcp/registry.rs and mcp/to_acp.rs already carry). The approved set, not
/// the manifest, is authoritative.
pub struct CapabilityStore {
    file_path: PathBuf,
    map: Mutex<StoreShape>,
}

impl CapabilityStore {
    pub fn new(file_path: PathBuf) -> Self {
        let map = Self::load(&file_path);
        Self {
            file_path,
            map: Mutex::new(map),
        }
    }

    /// Approved https origins for an app (empty if none).
    pub fn approved(&self, name: &str) -> Vec<String> {
        self.map
            .lock()
            .unwrap()
            .get(name)
            .map(|e| e.approved.clone())
            .unwrap_or_default()
    }

    /// Approve a set of hosts for an app. Non-https/invalid hosts are
    /// ignored.
    pub fn approve(&self, name: &str, hosts: &[String]) -> Result<(), String> {
        let name = assert_app_name(name)?;
        let valid: Vec<String> = hosts.iter().filter_map(|h| normalize_host(h)).collect();
        if valid.is_empty() {
            return Ok(());
        }
        let mut map = self.map.lock().unwrap();
        let entry = map.entry(name).or_default();
        let mut current: std::collections::BTreeSet<String> =
            entry.approved.iter().cloned().collect();
        for h in valid {
            current.insert(h);
        }
        entry.approved = current.into_iter().collect();
        self.persist(&map)
    }

    /// Revoke one host (or all, if `host` is `None`) for an app.
    pub fn revoke(&self, name: &str, host: Option<&str>) -> Result<(), String> {
        let name = assert_app_name(name)?;
        let mut map = self.map.lock().unwrap();
        if !map.contains_key(&name) {
            return Ok(());
        }
        match host {
            None => {
                map.remove(&name);
            }
            Some(host) => {
                let normalized = normalize_host(host).unwrap_or_else(|| host.to_string());
                if let Some(entry) = map.get_mut(&name) {
                    entry.approved.retain(|h| h != &normalized);
                }
            }
        }
        self.persist(&map)
    }

    /// Approved + pending (manifest-requested but not approved) for the UI.
    pub fn capabilities(&self, repo_root: &Path, name: &str) -> AppCapabilities {
        let approved = self.approved(name);
        let approved_set: std::collections::HashSet<&str> =
            approved.iter().map(String::as_str).collect();
        let pending = read_manifest(repo_root, name)
            .into_iter()
            .filter(|r| !approved_set.contains(r.host.as_str()))
            .collect();
        AppCapabilities { approved, pending }
    }

    fn load(file_path: &Path) -> StoreShape {
        let Ok(text) = fs::read_to_string(file_path) else {
            return StoreShape::new();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Write-through-temp-then-rename, matching mcp/registry.rs's own
    /// pattern — a concurrent reader always sees either the old or the new
    /// file whole, never a partial write.
    fn persist(&self, map: &StoreShape) -> Result<(), String> {
        let dir = self
            .file_path
            .parent()
            .ok_or("capability store path has no parent directory")?;
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
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
    use std::fs;

    fn tmp_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("micro-app-capabilities.json");
        (dir, path)
    }

    #[test]
    fn normalize_host_accepts_a_bare_https_origin() {
        assert_eq!(
            normalize_host("https://www.googleapis.com"),
            Some("https://www.googleapis.com".to_string())
        );
    }

    #[test]
    fn normalize_host_accepts_a_trailing_slash() {
        assert_eq!(
            normalize_host("https://api.example.com/"),
            Some("https://api.example.com".to_string())
        );
    }

    #[test]
    fn normalize_host_rejects_http() {
        assert_eq!(normalize_host("http://api.example.com"), None);
    }

    #[test]
    fn normalize_host_rejects_a_path() {
        assert_eq!(normalize_host("https://api.example.com/v1"), None);
    }

    #[test]
    fn normalize_host_rejects_a_query_string() {
        assert_eq!(normalize_host("https://api.example.com/?x=1"), None);
    }

    #[test]
    fn normalize_host_rejects_embedded_credentials() {
        assert_eq!(normalize_host("https://user:pass@api.example.com"), None);
    }

    #[test]
    fn normalize_host_rejects_loopback() {
        assert_eq!(normalize_host("https://localhost"), None);
        assert_eq!(normalize_host("https://127.0.0.1"), None);
    }

    #[test]
    fn normalize_host_rejects_a_bare_single_label() {
        assert_eq!(normalize_host("https://internal"), None);
    }

    #[test]
    fn normalize_host_rejects_garbage() {
        assert_eq!(normalize_host("not a url"), None);
        assert_eq!(normalize_host(""), None);
    }

    #[test]
    fn normalize_host_lowercases_the_host() {
        assert_eq!(
            normalize_host("https://API.Example.com"),
            Some("https://api.example.com".to_string())
        );
    }

    #[test]
    fn store_approve_persists_and_reloads() {
        let (_dir, file) = tmp_file();
        let store = CapabilityStore::new(file.clone());
        store
            .approve("my-app", &["https://api.example.com".to_string()])
            .unwrap();
        assert_eq!(store.approved("my-app"), vec!["https://api.example.com"]);
        let reloaded = CapabilityStore::new(file);
        assert_eq!(reloaded.approved("my-app"), vec!["https://api.example.com"]);
    }

    #[test]
    fn store_approve_ignores_invalid_hosts() {
        let (_dir, file) = tmp_file();
        let store = CapabilityStore::new(file);
        store.approve("my-app", &["not-https".to_string()]).unwrap();
        assert!(store.approved("my-app").is_empty());
    }

    #[test]
    fn store_revoke_one_host_keeps_the_rest() {
        let (_dir, file) = tmp_file();
        let store = CapabilityStore::new(file);
        store
            .approve(
                "my-app",
                &[
                    "https://a.example.com".to_string(),
                    "https://b.example.com".to_string(),
                ],
            )
            .unwrap();
        store
            .revoke("my-app", Some("https://a.example.com"))
            .unwrap();
        assert_eq!(store.approved("my-app"), vec!["https://b.example.com"]);
    }

    #[test]
    fn store_revoke_all_when_host_omitted() {
        let (_dir, file) = tmp_file();
        let store = CapabilityStore::new(file);
        store
            .approve("my-app", &["https://a.example.com".to_string()])
            .unwrap();
        store.revoke("my-app", None).unwrap();
        assert!(store.approved("my-app").is_empty());
    }

    #[test]
    fn read_manifest_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_manifest(dir.path(), "my-app").is_empty());
    }

    #[test]
    fn read_manifest_drops_invalid_hosts_but_keeps_valid_ones() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = dir.path().join("micro-apps").join("my-app");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(
            app_dir.join("hearth.app.json"),
            serde_json::json!({
                "hosts": [
                    { "host": "https://good.example.com", "reason": "sync" },
                    { "host": "not-https", "reason": "bad" },
                ]
            })
            .to_string(),
        )
        .unwrap();
        let reqs = read_manifest(dir.path(), "my-app");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].host, "https://good.example.com");
    }

    #[test]
    fn capabilities_excludes_already_approved_hosts_from_pending() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = dir.path().join("micro-apps").join("my-app");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(
            app_dir.join("hearth.app.json"),
            serde_json::json!({ "hosts": [{ "host": "https://a.example.com", "reason": "" }] })
                .to_string(),
        )
        .unwrap();
        let (_store_dir, file) = tmp_file();
        let store = CapabilityStore::new(file);
        store
            .approve("my-app", &["https://a.example.com".to_string()])
            .unwrap();
        let caps = store.capabilities(dir.path(), "my-app");
        assert_eq!(caps.approved, vec!["https://a.example.com"]);
        assert!(caps.pending.is_empty());
    }

    #[test]
    fn capabilities_rejects_traversal_in_name() {
        let dir = tempfile::tempdir().unwrap();
        let (_store_dir, file) = tmp_file();
        let store = CapabilityStore::new(file);
        assert!(store
            .approve("../../etc", &["https://a.example.com".to_string()])
            .is_err());
        let caps = store.capabilities(dir.path(), "my-app");
        assert!(caps.approved.is_empty());
    }
}
