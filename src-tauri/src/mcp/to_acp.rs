// Ported from electron/main/mcp/to-acp.ts (Phase 5, tracking issue #27).
// Maps Hearth's MCP server configs into the ACP wire shape
// (`agent_client_protocol::schema::v1::McpServer`), resolving secret
// references against the secret store. Pure (the store is injected via
// `SecretLookup`, spec #26's "one injected trait per subsystem" testing
// decision) so it's unit-testable. Disabled servers and servers with
// unresolved secrets are dropped — a half-configured server should not
// silently launch without its credentials.
//
// `SecretLookup` has no real backing implementation yet: secrets storage
// (`safeStorage` -> `keyring`) is explicitly out of scope for this MVP (spec
// #26's Out of Scope), the same standing gap Phase 3 already left in the
// auth path (see `agent_commands.rs`). Every `secretKey`-bound env var
// therefore reports as missing until that follow-on lands — an honest
// "missing secret" rather than a silent bypass.

use super::registry::{McpEnvVar, McpServerConfig, McpTransport};
use agent_client_protocol::schema::v1::{
    EnvVariable, HttpHeader, McpServer, McpServerHttp, McpServerSse, McpServerStdio,
};

/// DI seam for secret resolution (spec #26 Testing Decisions).
pub trait SecretLookup: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkippedServer {
    pub name: String,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToAcpResult {
    pub servers: Vec<McpServer>,
    /// Enabled servers skipped because a referenced secret was missing.
    pub skipped: Vec<SkippedServer>,
}

/// Resolve one env var/header to a concrete value, or `None` if a secret is
/// missing.
fn resolve_var(v: &McpEnvVar, secrets: &dyn SecretLookup) -> Option<(String, String)> {
    if let Some(key) = &v.secret_key {
        secrets.get(key).map(|value| (v.name.clone(), value))
    } else {
        Some((v.name.clone(), v.value.clone().unwrap_or_default()))
    }
}

pub fn to_acp_servers(configs: &[McpServerConfig], secrets: &dyn SecretLookup) -> ToAcpResult {
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    for cfg in configs {
        if !cfg.enabled {
            continue;
        }

        let mut vars = Vec::new();
        let mut missing = Vec::new();
        for v in &cfg.env {
            match resolve_var(v, secrets) {
                Some(pair) => vars.push(pair),
                None => missing.push(v.secret_key.clone().unwrap_or_else(|| v.name.clone())),
            }
        }
        if !missing.is_empty() {
            skipped.push(SkippedServer {
                name: cfg.name.clone(),
                missing,
            });
            continue;
        }

        match &cfg.transport {
            McpTransport::Stdio { command, args } => {
                servers.push(McpServer::Stdio(
                    McpServerStdio::new(cfg.name.clone(), command.clone())
                        .args(args.clone())
                        .env(
                            vars.into_iter()
                                .map(|(n, v)| EnvVariable::new(n, v))
                                .collect(),
                        ),
                ));
            }
            McpTransport::Http { url } => {
                servers.push(McpServer::Http(
                    McpServerHttp::new(cfg.name.clone(), url.clone()).headers(
                        vars.into_iter()
                            .map(|(n, v)| HttpHeader::new(n, v))
                            .collect(),
                    ),
                ));
            }
            McpTransport::Sse { url } => {
                servers.push(McpServer::Sse(
                    McpServerSse::new(cfg.name.clone(), url.clone()).headers(
                        vars.into_iter()
                            .map(|(n, v)| HttpHeader::new(n, v))
                            .collect(),
                    ),
                ));
            }
        }
    }

    ToAcpResult { servers, skipped }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct FakeSecrets(HashMap<String, String>);
    impl SecretLookup for FakeSecrets {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    fn secrets(pairs: &[(&str, &str)]) -> FakeSecrets {
        FakeSecrets(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn cfg(name: &str, transport: McpTransport, env: Vec<McpEnvVar>) -> McpServerConfig {
        McpServerConfig {
            id: "1".to_string(),
            name: name.to_string(),
            enabled: true,
            transport,
            env,
        }
    }

    #[test]
    fn maps_stdio_with_resolved_secret_env() {
        let c = cfg(
            "Foo",
            McpTransport::Stdio {
                command: "foo".to_string(),
                args: vec!["-x".to_string()],
            },
            vec![McpEnvVar {
                name: "TOKEN".to_string(),
                secret_key: Some("mcp.foo.TOKEN".to_string()),
                value: None,
            }],
        );
        let result = to_acp_servers(&[c], &secrets(&[("mcp.foo.TOKEN", "sekret")]));
        assert!(result.skipped.is_empty());
        match &result.servers[0] {
            McpServer::Stdio(s) => {
                assert_eq!(s.name, "Foo");
                assert_eq!(s.command.to_string_lossy(), "foo");
                assert_eq!(s.args, vec!["-x".to_string()]);
                assert_eq!(s.env.len(), 1);
                assert_eq!(s.env[0].name, "TOKEN");
                assert_eq!(s.env[0].value, "sekret");
            }
            other => panic!("expected Stdio, got {other:?}"),
        }
    }

    #[test]
    fn drops_disabled_servers() {
        let c = cfg(
            "Off",
            McpTransport::Stdio {
                command: "foo".to_string(),
                args: vec![],
            },
            vec![],
        );
        let c = McpServerConfig {
            enabled: false,
            ..c
        };
        assert!(to_acp_servers(&[c], &secrets(&[])).servers.is_empty());
    }

    #[test]
    fn skips_enabled_server_with_a_missing_secret_rather_than_launching_it_bare() {
        let c = cfg(
            "NeedsToken",
            McpTransport::Stdio {
                command: "foo".to_string(),
                args: vec![],
            },
            vec![McpEnvVar {
                name: "TOKEN".to_string(),
                secret_key: Some("mcp.foo.TOKEN".to_string()),
                value: None,
            }],
        );
        let result = to_acp_servers(&[c], &secrets(&[]));
        assert!(result.servers.is_empty());
        assert_eq!(
            result.skipped,
            vec![SkippedServer {
                name: "NeedsToken".to_string(),
                missing: vec!["mcp.foo.TOKEN".to_string()]
            }]
        );
    }

    #[test]
    fn maps_http_transport_to_headers_with_a_type_discriminator() {
        let c = cfg(
            "Remote",
            McpTransport::Http {
                url: "https://example.com/mcp".to_string(),
            },
            vec![McpEnvVar {
                name: "Authorization".to_string(),
                secret_key: None,
                value: Some("Bearer xyz".to_string()),
            }],
        );
        let result = to_acp_servers(&[c], &secrets(&[]));
        match &result.servers[0] {
            McpServer::Http(h) => {
                assert_eq!(h.name, "Remote");
                assert_eq!(h.url, "https://example.com/mcp");
                assert_eq!(h.headers.len(), 1);
                assert_eq!(h.headers[0].name, "Authorization");
                assert_eq!(h.headers[0].value, "Bearer xyz");
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[test]
    fn maps_sse_transport_to_headers() {
        let c = cfg(
            "Remote",
            McpTransport::Sse {
                url: "https://example.com/sse".to_string(),
            },
            vec![],
        );
        let result = to_acp_servers(&[c], &secrets(&[]));
        match &result.servers[0] {
            McpServer::Sse(s) => {
                assert_eq!(s.name, "Remote");
                assert_eq!(s.url, "https://example.com/sse");
                assert!(s.headers.is_empty());
            }
            other => panic!("expected Sse, got {other:?}"),
        }
    }

    #[test]
    fn a_literal_value_env_var_needs_no_secret_lookup() {
        let c = cfg(
            "Foo",
            McpTransport::Stdio {
                command: "foo".to_string(),
                args: vec![],
            },
            vec![McpEnvVar {
                name: "MODE".to_string(),
                secret_key: None,
                value: Some("prod".to_string()),
            }],
        );
        let result = to_acp_servers(&[c], &secrets(&[]));
        assert!(result.skipped.is_empty());
    }
}
