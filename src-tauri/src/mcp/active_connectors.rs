// Ported from electron/main/mcp/active-connectors.ts (Phase 5, tracking
// issue #27). Read-only view of the MCP connectors each backend will
// actually load. The ACP adapters merge the user's own CLI config into every
// Hearth session (Claude: ~/.claude.json user-scope + per-project
// local-scope + project .mcp.json; Codex: ~/.codex/config.toml). Hearth
// doesn't manage these — the CLIs do — so this is strictly read-only: we
// surface name + transport + scope so the user can SEE what's active and
// trust it. We never read, store, or log auth values (headers/env/tokens) —
// only whether auth is configured at all. See docs/COMPLIANCE.md.
//
// This is the real caller `terminal::login_path::LoginPathResolver::cli_resolves`
// was ported for in Phase 4 (carrying `#[allow(dead_code)]` until now — see
// that module's own header comment).
//
// One ordering divergence from the TS original, deliberately accepted: Claude's
// `mcpServers` object is parsed here via `serde_json::Value`, whose default
// `Map` (no `preserve_order` feature) iterates alphabetically rather than in
// original JSON-source order like a JS object would. Neither the TS tests nor
// the ported ones assert on list order, and this only affects the connectors
// panel's display order, not which connectors show up or their fields.

use crate::terminal::login_path::{LoginPathResolver, ShellQuery};
use crate::terminal::pty::default_shell;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorScope {
    User,
    Project,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorTransport {
    Stdio,
    Http,
    Sse,
}

/// One MCP connector a backend will load, surfaced read-only (managed by the
/// CLI, not by Hearth). Auth values are never included — only whether auth
/// is set.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveConnector {
    pub name: String,
    pub scope: ConnectorScope,
    pub transport: ConnectorTransport,
    /// Non-secret target: the URL (http/sse) or the command (stdio).
    pub target: String,
    /// Whether auth (headers/env) is configured — presence only, never the
    /// value.
    pub has_auth: bool,
}

/// Read-only snapshot of what each backend loads, plus whether each CLI
/// resolves on the PATH (drives detect-and-hint when claude/codex aren't
/// installed).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveConnectors {
    pub claude: Vec<ActiveConnector>,
    pub codex: Vec<ActiveConnector>,
    pub claude_cli: bool,
    pub codex_cli: bool,
}

/// Read what both backends will load for a given workspace. Never panics —
/// every filesystem/parse step degrades to an empty result on failure.
///
/// `resolver` is injected rather than built fresh per call: `LoginPathResolver`
/// caches its login-shell PATH resolution internally (`login-path.ts`'s own
/// header comment: "we resolve the user's real login PATH ONCE... Spawning a
/// login shell per terminal would re-source heavy rc files every time; once
/// is enough") — a resolver built inside this function would start that cache
/// empty on every single `connectors_active` call, defeating the point.
/// Callers should hold one long-lived resolver (see `McpState` in
/// `mcp_commands.rs`) and pass it in each time, the same way `TerminalManager`
/// already owns its own long-lived resolver.
pub fn read_active_connectors<Q: ShellQuery>(
    cwd: &Path,
    resolver: &LoginPathResolver<Q>,
) -> ActiveConnectors {
    let base_env: HashMap<String, String> = std::env::vars().collect();
    let shell = default_shell(&base_env);
    ActiveConnectors {
        claude: read_claude(cwd),
        codex: read_codex(),
        claude_cli: resolver.cli_resolves("claude", &base_env, &shell),
        codex_cli: resolver.cli_resolves("codex", &base_env, &shell),
    }
}

fn read_json(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

// ── Claude: ~/.claude.json (+ project .mcp.json) ────────────────────────────
// user scope  = top-level `mcpServers`
// local scope = `projects["<cwd>"].mcpServers`
// project     = `<cwd>/.mcp.json` -> `mcpServers`
fn read_claude(cwd: &Path) -> Vec<ActiveConnector> {
    let root = dirs::home_dir()
        .map(|h| h.join(".claude.json"))
        .and_then(|p| read_json(&p))
        .unwrap_or(Value::Null);
    let project_servers =
        read_json(&cwd.join(".mcp.json")).and_then(|v| v.get("mcpServers").cloned());
    parse_claude_connectors(&root, cwd, project_servers.as_ref())
}

/// Pure parse of Claude's config objects into connectors (testable, no IO).
pub fn parse_claude_connectors(
    root: &Value,
    cwd: &Path,
    project_servers: Option<&Value>,
) -> Vec<ActiveConnector> {
    let mut out = Vec::new();
    collect(&mut out, root.get("mcpServers"), ConnectorScope::User);
    let cwd_str = cwd.to_string_lossy();
    let local = root
        .get("projects")
        .and_then(|p| p.get(cwd_str.as_ref()))
        .and_then(|p| p.get("mcpServers"));
    collect(&mut out, local, ConnectorScope::Local);
    collect(&mut out, project_servers, ConnectorScope::Project);
    out
}

fn collect(out: &mut Vec<ActiveConnector>, servers: Option<&Value>, scope: ConnectorScope) {
    let Some(Value::Object(map)) = servers else {
        return;
    };
    for (name, raw) in map {
        let s = raw.as_object();
        let url = s.and_then(|m| m.get("url")).and_then(|v| v.as_str());
        let declared = s
            .and_then(|m| m.get("type").or_else(|| m.get("transport")))
            .and_then(|v| v.as_str());
        let transport = match declared {
            Some("http") => ConnectorTransport::Http,
            Some("sse") => ConnectorTransport::Sse,
            _ if url.is_some() => ConnectorTransport::Http,
            _ => ConnectorTransport::Stdio,
        };
        let command = s.and_then(|m| m.get("command")).and_then(|v| v.as_str());
        let has_auth = s
            .map(|m| has_entries(m.get("headers")) || has_entries(m.get("env")))
            .unwrap_or(false);
        out.push(ActiveConnector {
            name: name.clone(),
            scope,
            transport,
            target: url.or(command).unwrap_or("").to_string(),
            has_auth,
        });
    }
}

fn has_entries(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Object(m)) if !m.is_empty())
}

// ── Codex: ~/.codex/config.toml ─────────────────────────────────────────────
// Minimal reader for `[mcp_servers.<name>]` tables only — NOT a general TOML
// parser. We extract the keys we display (command / url / transport) and
// detect whether env/headers are present; unknown lines are ignored so it
// degrades gracefully rather than failing on TOML we don't model. Hand-rolled
// line parsing rather than pulling in a TOML crate or `fancy-regex` for a
// pattern this narrow — same minimal-dependency call the rest of this port
// makes elsewhere.
fn read_codex() -> Vec<ActiveConnector> {
    let Some(path) = dirs::home_dir().map(|h| h.join(".codex").join("config.toml")) else {
        return Vec::new();
    };
    match fs::read_to_string(&path) {
        Ok(text) => parse_codex_connectors(&text),
        Err(_) => Vec::new(),
    }
}

struct CodexServer {
    url: Option<String>,
    command: Option<String>,
    transport: Option<String>,
    auth: bool,
}

/// Pure parse of Codex's config.toml into connectors (testable, no IO).
/// Minimal `[mcp_servers.<name>]` reader — see the module header; not a
/// general TOML parser.
pub fn parse_codex_connectors(text: &str) -> Vec<ActiveConnector> {
    let mut servers: Vec<(String, CodexServer)> = Vec::new();
    let mut current: Option<usize> = None; // index into `servers`
    let mut in_sub = false; // inside a `[mcp_servers.<name>.<sub>]` subtable

    for raw_line in text.lines() {
        let line = strip_comment(raw_line).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[') {
            let inner = inner.trim_end_matches(']').trim_start_matches('[').trim();
            match parse_table_header(inner) {
                Some((name, sub)) => {
                    let idx = match servers.iter().position(|(n, _)| n == &name) {
                        Some(i) => i,
                        None => {
                            servers.push((
                                name,
                                CodexServer {
                                    url: None,
                                    command: None,
                                    transport: None,
                                    auth: false,
                                },
                            ));
                            servers.len() - 1
                        }
                    };
                    current = Some(idx);
                    in_sub = sub.is_some();
                    if let Some(sub) = &sub {
                        if sub.starts_with("env") || sub.starts_with("headers") {
                            servers[idx].1.auth = true;
                        }
                    }
                }
                None => {
                    current = None;
                    in_sub = false;
                }
            }
            continue;
        }
        let Some(idx) = current else { continue };
        if in_sub {
            continue; // keys belong to a subtable; the header already recorded auth
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let val = unquote(val.trim());
        let s = &mut servers[idx].1;
        match key {
            "url" => s.url = Some(val),
            "command" => s.command = Some(val),
            "transport" | "type" => s.transport = Some(val),
            "env" | "headers" => s.auth = true,
            k if k.starts_with("env.") || k.starts_with("headers.") => s.auth = true,
            _ => {}
        }
    }

    servers
        .into_iter()
        .map(|(name, s)| {
            let transport = match s.transport.as_deref() {
                Some("http") => ConnectorTransport::Http,
                Some("sse") => ConnectorTransport::Sse,
                _ if s.url.is_some() => ConnectorTransport::Http,
                _ => ConnectorTransport::Stdio,
            };
            ActiveConnector {
                name,
                scope: ConnectorScope::User,
                transport,
                target: s.url.or(s.command).unwrap_or_default(),
                has_auth: s.auth,
            }
        })
        .collect()
}

/// `mcp_servers.<name>` or `mcp_servers.<name>.<sub>` -> `(name, sub)`. Any
/// other table header (or a malformed `mcp_servers.*` one) is `None`.
fn parse_table_header(inner: &str) -> Option<(String, Option<String>)> {
    let rest = inner.strip_prefix("mcp_servers.")?;
    let (name, sub) = match rest.split_once('.') {
        Some((n, s)) => (n, Some(s.to_string())),
        None => (rest, None),
    };
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some((name.to_string(), sub))
}

/// Drop an unquoted trailing comment. Good enough for the keys we read.
fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn unquote(v: &str) -> String {
    let bytes = v.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- read_active_connectors, against an injected fake ShellQuery ---

    struct FakeShellQuery {
        resolves: Vec<String>,
    }
    impl ShellQuery for FakeShellQuery {
        fn resolve_login_path(&self, _shell: &str) -> Option<String> {
            None
        }
        fn which(&self, name: &str, _env: &HashMap<String, String>) -> bool {
            self.resolves.iter().any(|n| n == name)
        }
    }

    #[test]
    fn claude_cli_and_codex_cli_are_wired_from_the_injected_resolver() {
        let resolver = LoginPathResolver::new(
            FakeShellQuery {
                resolves: vec!["claude".to_string()],
            },
            false,
        );
        let out = read_active_connectors(Path::new("/repo"), &resolver);
        assert!(out.claude_cli);
        assert!(!out.codex_cli);
    }

    #[test]
    fn parse_claude_reads_user_local_and_project_scopes_with_transport_and_auth_presence() {
        let root = json!({
            "mcpServers": {
                "notion": { "type": "http", "url": "https://mcp.notion.com/mcp", "headers": { "Authorization": "Bearer secret" } },
                "local_tool": { "command": "npx", "args": ["-y", "thing"] },
            },
            "projects": {
                "/repo": { "mcpServers": { "scoped": { "type": "sse", "url": "https://x/sse" } } },
                "/other": { "mcpServers": { "ignored": { "command": "nope" } } },
            },
        });
        let project_servers =
            json!({ "fromMcpJson": { "url": "https://y/mcp", "env": { "K": "v" } } });
        let out = parse_claude_connectors(&root, Path::new("/repo"), Some(&project_servers));

        let notion = out.iter().find(|c| c.name == "notion").unwrap();
        assert_eq!(notion.scope, ConnectorScope::User);
        assert_eq!(notion.transport, ConnectorTransport::Http);
        assert_eq!(notion.target, "https://mcp.notion.com/mcp");
        assert!(notion.has_auth);

        // stdio inferred when no url/type; no auth
        let local_tool = out.iter().find(|c| c.name == "local_tool").unwrap();
        assert_eq!(local_tool.scope, ConnectorScope::User);
        assert_eq!(local_tool.transport, ConnectorTransport::Stdio);
        assert!(!local_tool.has_auth);

        // local scope from projects[cwd]; the other project is not included
        let scoped = out.iter().find(|c| c.name == "scoped").unwrap();
        assert_eq!(scoped.scope, ConnectorScope::Local);
        assert_eq!(scoped.transport, ConnectorTransport::Sse);
        assert!(out.iter().all(|c| c.name != "ignored"));

        // project scope from .mcp.json; env counts as auth
        let from_mcp_json = out.iter().find(|c| c.name == "fromMcpJson").unwrap();
        assert_eq!(from_mcp_json.scope, ConnectorScope::Project);
        assert_eq!(from_mcp_json.transport, ConnectorTransport::Http);
        assert!(from_mcp_json.has_auth);
    }

    #[test]
    fn parse_claude_empty_or_missing_config_yields_no_connectors() {
        assert!(parse_claude_connectors(&Value::Null, Path::new("/repo"), None).is_empty());
        assert!(parse_claude_connectors(&json!({}), Path::new("/repo"), None).is_empty());
    }

    #[test]
    fn parse_codex_reads_mcp_servers_tables_infers_transport_detects_auth() {
        let toml = r#"
model = "o3"

[mcp_servers.notion]
url = "https://mcp.notion.com/mcp"
transport = "http"

[mcp_servers.docs]
command = "npx"
args = ["-y", "@some/mcp"]

[mcp_servers.secured]
url = "https://x/mcp"  # inline comment
[mcp_servers.secured.env]
TOKEN = "shh"

[some_other_table]
url = "https://not-a-connector"
"#;
        let out = parse_codex_connectors(toml);

        let notion = out.iter().find(|c| c.name == "notion").unwrap();
        assert_eq!(notion.scope, ConnectorScope::User);
        assert_eq!(notion.transport, ConnectorTransport::Http);
        assert_eq!(notion.target, "https://mcp.notion.com/mcp");
        assert!(!notion.has_auth);

        let docs = out.iter().find(|c| c.name == "docs").unwrap();
        assert_eq!(docs.transport, ConnectorTransport::Stdio);
        assert_eq!(docs.target, "npx");
        assert!(!docs.has_auth);

        // nested [mcp_servers.secured.env] table marks auth present; comment stripped
        let secured = out.iter().find(|c| c.name == "secured").unwrap();
        assert_eq!(secured.transport, ConnectorTransport::Http);
        assert_eq!(secured.target, "https://x/mcp");
        assert!(secured.has_auth);

        // keys under an unrelated table are not attributed to a connector
        assert!(out.iter().all(|c| c.target != "https://not-a-connector"));
    }

    #[test]
    fn parse_codex_nested_env_subtable_is_not_a_phantom_server() {
        let toml = r#"
[mcp_servers.node_repl]
command = "node"

[mcp_servers.node_repl.env]
NODE_OPTIONS = "--x"
"#;
        let out = parse_codex_connectors(toml);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "node_repl");
        assert_eq!(out[0].transport, ConnectorTransport::Stdio);
        assert!(out[0].has_auth);
        assert!(out.iter().all(|c| c.name != "node_repl.env"));
    }

    #[test]
    fn parse_codex_empty_config_yields_no_connectors() {
        assert!(parse_codex_connectors("").is_empty());
    }
}
