// Ported from electron/main/micro-apps/broker.ts (Phase 6, tracking issue
// #27). Credential broker (W7 of the sandbox-hardening plan).
//
// A micro-app frame must never hold a raw secret: it is untrusted, and an
// opaque-origin sandboxed frame that held an OAuth token could exfiltrate
// it. So a frame that needs to call an authed external API does NOT call it
// directly — it calls this loopback broker, which (1) checks the caller's
// per-app token, (2) enforces the app's user-approved host allowlist (W6),
// (3) injects the credential from a secret store server-side, and (4)
// forwards the request. The token the frame holds only grants reach to that
// app's already-approved hosts; the actual secret never crosses into
// renderer-readable space.
//
// Sync (tiny_http + ureq), matching bridge.rs's own loopback server —
// csp_proxy.rs is the one async subsystem in micro_apps/, needed there
// specifically for the WS-upgrade splice.

use crate::micro_apps::capabilities::{normalize_host, CapabilityStore};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use tiny_http::{Header, Method, Request, Response, Server};

/// Where the broker resolves a per-host secret from. Secrets storage is out
/// of scope for this MVP (spec #26's Out of Scope) — same standing gap
/// mcp/to_acp.rs's `NullSecretLookup` already carries for MCP servers.
pub trait SecretLookup: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
}

/// Always reports "no secret configured" — every `microapp.<origin>`-keyed
/// credential is honestly absent rather than silently bypassed, same
/// rationale as mcp/to_acp.rs's own `NullSecretLookup`.
pub struct NullSecretLookup;
impl SecretLookup for NullSecretLookup {
    fn get(&self, _key: &str) -> Option<String> {
        None
    }
}

pub enum ProxyDecision {
    Ok { target: String, origin: String },
    Err { status: u16, error: String },
}

/// Pure decision: given a request token + target URL, decide whether to
/// forward. Validates the token -> app, that the target is an https origin,
/// and that the origin is in that app's approved set. Unit-tested.
pub fn resolve_proxy(
    token: Option<&str>,
    target: Option<&str>,
    token_to_app: impl Fn(&str) -> Option<String>,
    approved_for: impl Fn(&str) -> Vec<String>,
) -> ProxyDecision {
    let Some(token) = token else {
        return ProxyDecision::Err {
            status: 401,
            error: "missing broker token".to_string(),
        };
    };
    let Some(app_name) = token_to_app(token) else {
        return ProxyDecision::Err {
            status: 401,
            error: "unknown broker token".to_string(),
        };
    };
    let Some(target) = target else {
        return ProxyDecision::Err {
            status: 400,
            error: "missing target".to_string(),
        };
    };
    let Ok(url) = url::Url::parse(target) else {
        return ProxyDecision::Err {
            status: 400,
            error: "invalid target URL".to_string(),
        };
    };
    let origin = url.origin().ascii_serialization();
    let Some(normalized) = normalize_host(&origin) else {
        return ProxyDecision::Err {
            status: 400,
            error: "target is not an allowed https origin".to_string(),
        };
    };
    if !approved_for(&app_name).contains(&normalized) {
        return ProxyDecision::Err {
            status: 403,
            error: format!("host not approved for {app_name}: {normalized}"),
        };
    }
    ProxyDecision::Ok {
        target: target.to_string(),
        origin: normalized,
    }
}

// Methods a frame may proxy. No TRACE/CONNECT.
const ALLOWED_METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
// Request headers passed through from the frame (never auth — the broker sets that).
const FORWARDABLE_REQ_HEADERS: [&str; 2] = ["content-type", "accept"];
// Caps to keep a hostile/agent-authored frame from ballooning memory.
const MAX_REQUEST_BYTES: u64 = 5 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Default)]
struct TokenMaps {
    token_to_app: HashMap<String, String>,
    app_to_token: HashMap<String, String>,
}

pub struct CredentialBroker {
    port: Mutex<Option<u16>>,
    tokens: Mutex<TokenMaps>,
    capabilities: Arc<CapabilityStore>,
    secrets: Arc<dyn SecretLookup>,
    agent: ureq::Agent,
}

impl CredentialBroker {
    pub fn new(capabilities: Arc<CapabilityStore>, secrets: Arc<dyn SecretLookup>) -> Self {
        // `redirects(0)`: a 3xx Location (attacker-chosen) must never be
        // followed automatically — see `forward`'s own handling below,
        // which turns a 3xx response into an explicit 502 rather than
        // silently relaying an attacker-chosen redirect with the injected
        // credential attached.
        let agent = ureq::AgentBuilder::new().redirects(0).build();
        Self {
            port: Mutex::new(None),
            tokens: Mutex::new(TokenMaps::default()),
            capabilities,
            secrets,
            agent,
        }
    }

    /// Start listening on a random loopback port. Takes `Arc<Self>` because
    /// the accept-loop thread needs its own owned handle back into the
    /// broker.
    pub fn start(self: &Arc<Self>) -> Result<(), String> {
        let server = Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
        let port = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| "broker: loopback server has no IP address".to_string())?
            .port();
        *self.port.lock().unwrap() = Some(port);

        let broker = self.clone();
        thread::spawn(move || {
            for request in server.incoming_requests() {
                broker.handle_request(request);
            }
        });
        Ok(())
    }

    /// The broker origin a frame calls and that must be in its CSP
    /// connect-src. `None` until `start` has run.
    pub fn origin(&self) -> Option<String> {
        self.port
            .lock()
            .unwrap()
            .map(|p| format!("http://127.0.0.1:{p}"))
    }

    /// Issue (or reuse) a per-app token. Tokens are not reachable across apps.
    pub fn token_for(&self, app_name: &str) -> String {
        let mut tokens = self.tokens.lock().unwrap();
        if let Some(existing) = tokens.app_to_token.get(app_name) {
            return existing.clone();
        }
        let token = generate_token();
        tokens
            .app_to_token
            .insert(app_name.to_string(), token.clone());
        tokens
            .token_to_app
            .insert(token.clone(), app_name.to_string());
        token
    }

    fn handle_request(&self, mut request: Request) {
        let method = request.method().clone();
        let url = request.url().to_string();

        if method == Method::Options {
            let mut resp = Response::empty(204);
            for h in cors_headers() {
                resp.add_header(h);
            }
            let _ = request.respond(resp);
            return;
        }
        if method != Method::Post || url != "/proxy" {
            respond_cors_text(request, 404, "not found");
            return;
        }

        let token = header_value(&request, "x-hearth-token");
        let target = header_value(&request, "x-hearth-target");
        let req_method = header_value(&request, "x-hearth-method")
            .unwrap_or_else(|| "GET".to_string())
            .to_uppercase();

        let tokens = self.tokens.lock().unwrap();
        let token_to_app: HashMap<String, String> = tokens.token_to_app.clone();
        drop(tokens);

        let decision = resolve_proxy(
            token.as_deref(),
            target.as_deref(),
            |t| token_to_app.get(t).cloned(),
            |app| self.capabilities.approved(app),
        );
        let (target, origin) = match decision {
            ProxyDecision::Err { status, error } => {
                respond_cors_text(request, status, &error);
                return;
            }
            ProxyDecision::Ok { target, origin } => (target, origin),
        };

        if !ALLOWED_METHODS.contains(&req_method.as_str()) {
            respond_cors_text(request, 405, "method not allowed");
            return;
        }

        let mut fwd_headers: Vec<(String, String)> = Vec::new();
        for name in FORWARDABLE_REQ_HEADERS {
            if let Some(v) = header_value(&request, name) {
                fwd_headers.push((name.to_string(), v));
            }
        }
        // Inject the credential server-side. The frame never sees it.
        if let Some(secret) = self.secrets.get(&format!("microapp.{origin}")) {
            fwd_headers.push(("authorization".to_string(), format!("Bearer {secret}")));
        }

        let body = if req_method != "GET" && req_method != "DELETE" {
            match read_capped(request.as_reader(), MAX_REQUEST_BYTES) {
                Ok(b) => Some(b),
                Err(_) => {
                    respond_cors_text(request, 413, "request body too large");
                    return;
                }
            }
        } else {
            None
        };

        match forward(&self.agent, &req_method, &target, &fwd_headers, body) {
            Ok((status, content_type, bytes)) => {
                let ct_header = Header::from_bytes(&b"content-type"[..], content_type.as_bytes())
                    .unwrap_or_else(|_| {
                        Header::from_bytes(&b"content-type"[..], &b"application/octet-stream"[..])
                            .unwrap()
                    });
                let cors_origin =
                    Header::from_bytes(&b"access-control-allow-origin"[..], &b"*"[..]).unwrap();
                let resp = Response::from_data(bytes)
                    .with_status_code(status)
                    .with_header(ct_header)
                    .with_header(cors_origin);
                let _ = request.respond(resp);
            }
            Err(msg) => respond_cors_text(request, 502, &msg),
        }
    }
}

fn cors_headers() -> Vec<Header> {
    vec![
        Header::from_bytes(&b"access-control-allow-origin"[..], &b"*"[..]).unwrap(),
        Header::from_bytes(
            &b"access-control-allow-headers"[..],
            &b"content-type, x-hearth-token, x-hearth-target, x-hearth-method"[..],
        )
        .unwrap(),
        Header::from_bytes(&b"access-control-allow-methods"[..], &b"POST, OPTIONS"[..]).unwrap(),
    ]
}

fn respond_cors_text(request: Request, status: u16, body: &str) {
    let mut resp = Response::from_string(body).with_status_code(status);
    for h in cors_headers() {
        resp.add_header(h);
    }
    let _ = request.respond(resp);
}

/// A verbatim copy of bridge.rs's own `header_value` — both are three-line
/// `tiny_http::Request` helpers in one crate, not worth sharing across a
/// module boundary neither file otherwise depends on.
fn header_value(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

/// Read up to `max` bytes, erroring if the stream has more. Used for both
/// the incoming frame request body and (via `forward`) the upstream
/// response body.
fn read_capped(mut reader: impl Read, max: u64) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let read = reader
        .by_ref()
        .take(max + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if read as u64 > max {
        return Err("body too large".to_string());
    }
    Ok(buf)
}

/// Forward one request to `target`. Returns `(status, content_type, body)`
/// on any non-redirect response (including 4xx/5xx — those are the
/// upstream's real answer, relayed transparently, not a broker error).
fn forward(
    agent: &ureq::Agent,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    body: Option<Vec<u8>>,
) -> Result<(u16, String, Vec<u8>), String> {
    let mut req = agent.request(method, target);
    for (name, value) in headers {
        req = req.set(name, value);
    }
    let result = match body {
        Some(b) => req.send_bytes(&b),
        None => req.call(),
    };
    let response = match result {
        Ok(resp) => resp,
        // `redirects(0)` on the agent means a 3xx never gets Err'd for
        // being a redirect — ureq only treats >=400 as Err — so a Status
        // error here really is an upstream 4xx/5xx, relayed as-is.
        Err(ureq::Error::Status(_, resp)) => resp,
        Err(e) => return Err(format!("upstream error: {e}")),
    };
    let status = response.status();
    // `redirect: 'manual'`'s Rust equivalent: refuse to relay a redirect
    // outright rather than follow an attacker-chosen Location with the
    // injected credential still attached.
    if (300..400).contains(&status) {
        return Err("upstream redirect not followed".to_string());
    }
    let content_type = response
        .header("content-type")
        .unwrap_or("application/octet-stream")
        .to_string();
    let reader = response.into_reader();
    let bytes = read_capped(reader, MAX_RESPONSE_BYTES)
        .map_err(|_| "upstream response too large".to_string())?;
    Ok((status, content_type, bytes))
}

/// 32 random bytes from /dev/urandom (Linux-only, per spec #26's scope
/// decision), hex encoded — mirrors bridge.rs's own `generate_token`,
/// kept as a separate local copy rather than shared (both are three lines).
fn generate_token() -> String {
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("broker: failed to read /dev/urandom for a per-app token");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;

    fn approved_map(entries: &[(&str, &[&str])]) -> StdHashMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect()
    }

    #[test]
    fn resolve_proxy_rejects_a_missing_token() {
        let decision = resolve_proxy(None, Some("https://api.example.com"), |_| None, |_| vec![]);
        assert!(matches!(decision, ProxyDecision::Err { status: 401, .. }));
    }

    #[test]
    fn resolve_proxy_rejects_an_unknown_token() {
        let decision = resolve_proxy(
            Some("bogus"),
            Some("https://api.example.com"),
            |_| None,
            |_| vec![],
        );
        assert!(matches!(decision, ProxyDecision::Err { status: 401, .. }));
    }

    #[test]
    fn resolve_proxy_rejects_a_missing_target() {
        let decision = resolve_proxy(
            Some("tok"),
            None,
            |_| Some("my-app".to_string()),
            |_| vec![],
        );
        assert!(matches!(decision, ProxyDecision::Err { status: 400, .. }));
    }

    #[test]
    fn resolve_proxy_rejects_a_non_https_target() {
        let decision = resolve_proxy(
            Some("tok"),
            Some("http://api.example.com"),
            |_| Some("my-app".to_string()),
            |_| vec![],
        );
        assert!(matches!(decision, ProxyDecision::Err { status: 400, .. }));
    }

    #[test]
    fn resolve_proxy_rejects_an_unapproved_host() {
        let approved = approved_map(&[("my-app", &["https://other.example.com"])]);
        let decision = resolve_proxy(
            Some("tok"),
            Some("https://api.example.com"),
            |_| Some("my-app".to_string()),
            |app| approved.get(app).cloned().unwrap_or_default(),
        );
        assert!(matches!(decision, ProxyDecision::Err { status: 403, .. }));
    }

    #[test]
    fn resolve_proxy_accepts_an_approved_host() {
        let approved = approved_map(&[("my-app", &["https://api.example.com"])]);
        let decision = resolve_proxy(
            Some("tok"),
            Some("https://api.example.com/v1/resource"),
            |_| Some("my-app".to_string()),
            |app| approved.get(app).cloned().unwrap_or_default(),
        );
        match decision {
            ProxyDecision::Ok { target, origin } => {
                assert_eq!(target, "https://api.example.com/v1/resource");
                assert_eq!(origin, "https://api.example.com");
            }
            ProxyDecision::Err { .. } => panic!("expected Ok"),
        }
    }

    #[test]
    fn null_secret_lookup_always_reports_absent() {
        assert_eq!(
            NullSecretLookup.get("microapp.https://api.example.com"),
            None
        );
    }

    #[test]
    fn broker_issues_a_stable_per_app_token() {
        let capabilities = Arc::new(CapabilityStore::new(
            tempfile::tempdir().unwrap().path().join("caps.json"),
        ));
        let broker = CredentialBroker::new(capabilities, Arc::new(NullSecretLookup));
        let a = broker.token_for("app-a");
        let b = broker.token_for("app-b");
        assert_ne!(a, b);
        assert_eq!(broker.token_for("app-a"), a);
    }

    #[test]
    fn broker_origin_is_none_until_started() {
        let capabilities = Arc::new(CapabilityStore::new(
            tempfile::tempdir().unwrap().path().join("caps.json"),
        ));
        let broker = CredentialBroker::new(capabilities, Arc::new(NullSecretLookup));
        assert_eq!(broker.origin(), None);
    }

    #[test]
    fn broker_start_binds_a_loopback_origin() {
        let capabilities = Arc::new(CapabilityStore::new(
            tempfile::tempdir().unwrap().path().join("caps.json"),
        ));
        let broker = Arc::new(CredentialBroker::new(
            capabilities,
            Arc::new(NullSecretLookup),
        ));
        broker.start().unwrap();
        let origin = broker.origin().unwrap();
        assert!(origin.starts_with("http://127.0.0.1:"));
    }
}
