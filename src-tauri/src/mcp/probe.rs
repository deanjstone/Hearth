// Ported from electron/main/mcp/probe.ts (Phase 5, tracking issue #27).
// "Test" for a configured MCP server: launch it and run the MCP handshake so
// the user learns it works (and how many tools it exposes) BEFORE a real
// session depends on it. stdio runs a full initialize + tools/list over the
// process's stdio (newline-delimited JSON-RPC). http/sse do a reachability
// check (a full streamable-HTTP handshake is out of scope for v1 — we report
// reachable, not a tool count, and say so honestly).
//
// The actual network/subprocess work sits behind an `McpProbe` trait (spec
// #26 Testing Decisions: "an McpProbe trait"), mirroring the DI seam every
// other ported subsystem uses (`PtySpawner`/`ShellQuery` in Phase 4,
// `ShellQuery` in login_path.rs). `probe_server`'s orchestration — secret
// resolution, the disabled/misconfigured/missing-secret branches — is tested
// against a fake; `RealMcpProbe`'s stdio/http implementations get their own
// real-subprocess/real-socket tests, matching `login_path.rs`'s
// `RealShellQuery` precedent.

use super::registry::McpServerConfig;
use super::to_acp::{to_acp_servers, SecretLookup};
use agent_client_protocol::schema::v1::{EnvVariable, HttpHeader, McpServer};
use async_trait::async_trait;
use serde::Serialize;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

const PROBE_TIMEOUT: Duration = Duration::from_millis(8000);

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub ok: bool,
    /// Number of tools the server advertised (stdio only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<u32>,
    /// True when we verified reachability but not a tool count (http/sse).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reachable_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ProbeResult {
    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            ..Default::default()
        }
    }
    fn ok_tools(n: u32) -> Self {
        Self {
            ok: true,
            tools: Some(n),
            ..Default::default()
        }
    }
    fn ok_reachable() -> Self {
        Self {
            ok: true,
            reachable_only: Some(true),
            ..Default::default()
        }
    }
}

/// DI seam for the actual network/subprocess probing.
#[async_trait]
pub trait McpProbe: Send + Sync {
    async fn stdio(&self, command: &Path, args: &[String], env: &[EnvVariable]) -> ProbeResult;
    async fn http(&self, url: &str, headers: &[HttpHeader]) -> ProbeResult;
}

/// The real OS-backed `McpProbe`.
pub struct RealMcpProbe;

#[async_trait]
impl McpProbe for RealMcpProbe {
    async fn stdio(&self, command: &Path, args: &[String], env: &[EnvVariable]) -> ProbeResult {
        probe_stdio(command, args, env).await
    }
    async fn http(&self, url: &str, headers: &[HttpHeader]) -> ProbeResult {
        probe_http(url, headers).await
    }
}

pub async fn probe_server(
    config: &McpServerConfig,
    secrets: &dyn SecretLookup,
    probe: &dyn McpProbe,
) -> ProbeResult {
    let single = McpServerConfig {
        enabled: true,
        ..config.clone()
    };
    let result = to_acp_servers(std::slice::from_ref(&single), secrets);
    if let Some(skip) = result.skipped.first() {
        return ProbeResult::err(format!("Missing secret(s): {}", skip.missing.join(", ")));
    }
    let Some(server) = result.servers.into_iter().next() else {
        return ProbeResult::err("Server is disabled or misconfigured");
    };
    match server {
        McpServer::Stdio(s) => probe.stdio(&s.command, &s.args, &s.env).await,
        McpServer::Http(h) => probe.http(&h.url, &h.headers).await,
        McpServer::Sse(s) => probe.http(&s.url, &s.headers).await,
        _ => ProbeResult::err("Unsupported MCP transport"),
    }
}

fn exited_message(status: std::io::Result<std::process::ExitStatus>) -> String {
    let code = status.ok().and_then(|s| s.code());
    let code_str = code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    format!("Server exited (code {code_str}) before responding")
}

fn error_message(err: &serde_json::Value, fallback: &str) -> String {
    err.get("message")
        .and_then(|m| m.as_str())
        .unwrap_or(fallback)
        .to_string()
}

async fn send_line(
    stdin: &mut tokio::process::ChildStdin,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value).expect("JSON-RPC frame always serializes");
    line.push('\n');
    stdin.write_all(line.as_bytes()).await
}

async fn probe_stdio(command: &Path, args: &[String], env: &[EnvVariable]) -> ProbeResult {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .envs(env.iter().map(|e| (e.name.clone(), e.value.clone())))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ProbeResult::err(e.to_string()),
    };
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut lines = BufReader::new(stdout).lines();

    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "hearth", "version": "1" },
        },
    });
    // A child that has already exited (or is about to) may fail this write
    // with a broken pipe — don't report that raw IO error as the cause; the
    // loop below observes the exit itself and reports a clearer message.
    let _ = send_line(&mut stdin, &init).await;

    let run = async {
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
                                continue // server log noise, not a JSON-RPC frame
                            };
                            match msg.get("id").and_then(|v| v.as_i64()) {
                                Some(1) => {
                                    // A JSON-RPC error to initialize (bad protocol version,
                                    // auth, etc.) is a real answer — report it instead of
                                    // hanging until the timeout.
                                    if let Some(err) = msg.get("error") {
                                        return ProbeResult::err(error_message(err, "initialize failed"));
                                    } else if msg.get("result").is_some() {
                                        let ack = serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
                                        if let Err(e) = send_line(&mut stdin, &ack).await {
                                            return ProbeResult::err(e.to_string());
                                        }
                                        let list = serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} });
                                        if let Err(e) = send_line(&mut stdin, &list).await {
                                            return ProbeResult::err(e.to_string());
                                        }
                                    }
                                }
                                Some(2) => {
                                    if let Some(err) = msg.get("error") {
                                        return ProbeResult::err(error_message(err, "tools/list failed"));
                                    }
                                    let tools = msg
                                        .get("result")
                                        .and_then(|r| r.get("tools"))
                                        .and_then(|t| t.as_array())
                                        .map(|a| a.len())
                                        .unwrap_or(0);
                                    return ProbeResult::ok_tools(tools as u32);
                                }
                                _ => continue,
                            }
                        }
                        // EOF usually means the process is exiting (or has
                        // exited) right around now — `child.wait()` picks up
                        // the real exit code instead of a vaguer "closed its
                        // output" message.
                        Ok(None) => return ProbeResult::err(exited_message(child.wait().await)),
                        Err(e) => return ProbeResult::err(e.to_string()),
                    }
                }
                status = child.wait() => {
                    return ProbeResult::err(exited_message(status));
                }
            }
        }
    };

    let result = match timeout(PROBE_TIMEOUT, run).await {
        Ok(r) => r,
        Err(_) => ProbeResult::err("Timed out waiting for the server"),
    };
    let _ = child.start_kill();
    result
}

async fn probe_http(url: &str, headers: &[HttpHeader]) -> ProbeResult {
    let url = url.to_string();
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|h| (h.name.clone(), h.value.clone()))
        .collect();
    match tokio::task::spawn_blocking(move || probe_http_blocking(&url, &headers)).await {
        Ok(r) => r,
        Err(e) => ProbeResult::err(format!("probe task failed: {e}")),
    }
}

/// Any HTTP response (even 4xx) means the endpoint is reachable; a network
/// failure/timeout does not. We don't parse the MCP body for http/sse in v1.
fn probe_http_blocking(url: &str, headers: &[(String, String)]) -> ProbeResult {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "hearth", "version": "1" },
        },
    });
    let mut req = ureq::post(url)
        .set("content-type", "application/json")
        .set("accept", "application/json, text/event-stream")
        .timeout(PROBE_TIMEOUT);
    for (name, value) in headers {
        req = req.set(name, value);
    }
    match req.send_json(body) {
        Ok(_) => ProbeResult::ok_reachable(),
        Err(ureq::Error::Status(code, _)) => {
            if code < 500 {
                ProbeResult::ok_reachable()
            } else {
                ProbeResult::err(format!("Server responded {code}"))
            }
        }
        Err(e) => ProbeResult::err(e.to_string()),
    }
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
    fn no_secrets() -> FakeSecrets {
        FakeSecrets(HashMap::new())
    }

    struct FakeProbe(ProbeResult);
    #[async_trait]
    impl McpProbe for FakeProbe {
        async fn stdio(
            &self,
            _command: &Path,
            _args: &[String],
            _env: &[EnvVariable],
        ) -> ProbeResult {
            self.0.clone()
        }
        async fn http(&self, _url: &str, _headers: &[HttpHeader]) -> ProbeResult {
            self.0.clone()
        }
    }

    use crate::mcp::registry::{McpEnvVar, McpTransport};

    fn stdio_config() -> McpServerConfig {
        McpServerConfig {
            id: "1".to_string(),
            name: "Fake".to_string(),
            enabled: true,
            transport: McpTransport::Stdio {
                command: "foo".to_string(),
                args: vec![],
            },
            env: vec![],
        }
    }

    // --- probe_server orchestration (against a fake McpProbe) ---

    #[tokio::test]
    async fn dispatches_to_stdio_probe_and_returns_its_result() {
        let probe = FakeProbe(ProbeResult {
            ok: true,
            tools: Some(3),
            ..Default::default()
        });
        let result = probe_server(&stdio_config(), &no_secrets(), &probe).await;
        assert_eq!(
            result,
            ProbeResult {
                ok: true,
                tools: Some(3),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn reports_missing_secret_without_ever_calling_the_probe() {
        let cfg = McpServerConfig {
            env: vec![McpEnvVar {
                name: "TOKEN".to_string(),
                secret_key: Some("k".to_string()),
                value: None,
            }],
            ..stdio_config()
        };
        let probe = FakeProbe(ProbeResult {
            ok: true,
            ..Default::default()
        });
        let result = probe_server(&cfg, &no_secrets(), &probe).await;
        assert!(!result.ok);
        assert_eq!(result.error.as_deref(), Some("Missing secret(s): k"));
    }

    #[tokio::test]
    async fn a_disabled_server_is_probed_anyway_since_probe_server_forces_it_enabled() {
        // Mirrors probe.ts's `{ ...config, enabled: true }` — testing a
        // server should work even if the user hasn't flipped it on yet.
        let cfg = McpServerConfig {
            enabled: false,
            ..stdio_config()
        };
        let probe = FakeProbe(ProbeResult {
            ok: true,
            ..Default::default()
        });
        let result = probe_server(&cfg, &no_secrets(), &probe).await;
        assert!(result.ok);
    }

    // --- RealMcpProbe::stdio, against real spawned `sh` fixtures ---

    #[tokio::test]
    async fn real_stdio_reports_a_json_rpc_error_to_initialize_instead_of_timing_out() {
        let script = r#"while IFS= read -r line; do
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"unsupported protocol version"}}' ;;
  esac
done"#;
        let start = std::time::Instant::now();
        let result = probe_stdio(
            Path::new("sh"),
            &["-c".to_string(), script.to_string()],
            &[],
        )
        .await;
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "should resolve well under the 8s probe timeout"
        );
        assert!(!result.ok);
        assert_eq!(
            result.error.as_deref(),
            Some("unsupported protocol version")
        );
    }

    #[tokio::test]
    async fn real_stdio_counts_tools_on_a_full_successful_handshake() {
        let script = r#"while IFS= read -r line; do
  case "$line" in
    *'"id":1'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{}}' ;;
    *'"id":2'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"a"},{"name":"b"},{"name":"c"}]}}' ;;
  esac
done"#;
        let result = probe_stdio(
            Path::new("sh"),
            &["-c".to_string(), script.to_string()],
            &[],
        )
        .await;
        assert_eq!(
            result,
            ProbeResult {
                ok: true,
                tools: Some(3),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn real_stdio_reports_an_early_exit_instead_of_hanging() {
        let result = probe_stdio(
            Path::new("sh"),
            &["-c".to_string(), "exit 3".to_string()],
            &[],
        )
        .await;
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("exited"));
    }

    #[tokio::test]
    async fn real_stdio_reports_a_spawn_failure_for_a_missing_binary() {
        let result = probe_stdio(
            Path::new("/does/not/exist/hearth-mcp-probe-fixture"),
            &[],
            &[],
        )
        .await;
        assert!(!result.ok);
        assert!(result.error.is_some());
    }

    // --- RealMcpProbe::http, against a real local tiny_http server ---

    #[tokio::test]
    async fn real_http_reports_reachable_on_a_2xx_response() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let handle = std::thread::spawn(move || {
            if let Ok(req) = server.recv() {
                let _ = req.respond(tiny_http::Response::from_string("{}").with_status_code(200));
            }
        });
        let result = probe_http(&format!("http://{addr}/"), &[]).await;
        handle.join().unwrap();
        assert_eq!(
            result,
            ProbeResult {
                ok: true,
                reachable_only: Some(true),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn real_http_reports_reachable_on_a_4xx_response() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let handle = std::thread::spawn(move || {
            if let Ok(req) = server.recv() {
                let _ = req.respond(tiny_http::Response::from_string("nope").with_status_code(404));
            }
        });
        let result = probe_http(&format!("http://{addr}/"), &[]).await;
        handle.join().unwrap();
        assert_eq!(
            result,
            ProbeResult {
                ok: true,
                reachable_only: Some(true),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn real_http_reports_failure_on_a_5xx_response() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr();
        let handle = std::thread::spawn(move || {
            if let Ok(req) = server.recv() {
                let _ = req.respond(tiny_http::Response::from_string("boom").with_status_code(500));
            }
        });
        let result = probe_http(&format!("http://{addr}/"), &[]).await;
        handle.join().unwrap();
        assert!(!result.ok);
        assert_eq!(result.error.as_deref(), Some("Server responded 500"));
    }

    #[tokio::test]
    async fn real_http_reports_failure_when_nothing_is_listening() {
        // Port 1 is a privileged port nothing binds to in a test sandbox —
        // the connection is refused, a transport-level failure.
        let result = probe_http("http://127.0.0.1:1/", &[]).await;
        assert!(!result.ok);
        assert!(result.error.is_some());
    }
}
