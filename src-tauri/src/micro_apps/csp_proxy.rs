// Ported from spike/tauri-csp-proxy/proxy/src/main.rs (wayfinder ticket #22,
// resolved by deanjstone/Hearth#20's locked design) into a reusable
// per-micro-app proxy for Phase 6 (tracking issue #27): dynamic upstream
// port (the app's own Vite dev server, chosen by server.rs), dynamic listen
// port (OS-assigned, so many apps' proxies can run at once), and a CSP
// computed per-request from the live `CapabilityStore` + broker origin —
// mirrors electron/main/micro-apps/session-policy.ts's `onHeadersReceived`
// hook reading `capabilities.approved(appName)` live, rather than the
// spike's single fixed `INJECTED_CSP` constant baked in at proxy-start time.
//
// wry/WebKitGTK has no API that rewrites response headers on real
// http(s):// traffic (deanjstone/Hearth#19/#20's locked finding, confirmed
// by wry#1087) — this proxy IS the enforcement point instead of a session
// hook. Every micro-app gets its OWN listener (not one shared, path-routed
// proxy) to keep the same-origin isolation between micro-apps that today's
// one-Vite-server-per-app topology already gives for free — #20 treats
// trading that away for simpler process management as a real regression,
// not a simplification.

use crate::micro_apps::capabilities::CapabilityStore;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderValue, CONNECTION, CONTENT_SECURITY_POLICY, UPGRADE};
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

/// Build the per-app micro-app CSP from its approved hosts. Ported from
/// session-policy.ts's `buildMicroAppCsp`. Pure + unit-tested.
pub fn build_micro_app_csp(
    self_origin: &str,
    approved_hosts: &[String],
    broker_origin: Option<&str>,
) -> String {
    // 'self' covers same-origin http(s); Vite HMR inside the frame needs its
    // own ws/wss origin spelled out.
    let ws_self = match self_origin.strip_prefix("http") {
        Some(rest) => format!("ws{rest}"),
        None => self_origin.to_string(),
    };
    let mut connect: Vec<String> = vec!["'self'".to_string(), ws_self];
    connect.extend(approved_hosts.iter().cloned());
    if let Some(broker) = broker_origin {
        connect.push(broker.to_string());
    }
    [
        "default-src 'self'".to_string(),
        "script-src 'self' 'unsafe-inline'".to_string(),
        "style-src 'self' 'unsafe-inline'".to_string(),
        "img-src 'self' data: blob:".to_string(),
        "font-src 'self' data:".to_string(),
        format!("connect-src {}", connect.join(" ")),
        "object-src 'none'".to_string(),
        "base-uri 'self'".to_string(),
        "frame-ancestors 'self'".to_string(),
    ]
    .join("; ")
}

struct ProxyState {
    self_origin: String,
    upstream_host: String,
    upstream_port: u16,
    app_name: String,
    capabilities: Arc<CapabilityStore>,
    broker_origin: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

/// What `start` needs beyond the upstream port: which app this proxy speaks
/// for (to look up its live approved-hosts list) and how to find the
/// broker's current origin (`None` until it's started — mirrors
/// electron/main/ipc.ts's `broker.origin()` null-until-started shape).
pub struct CspProxyDeps {
    pub app_name: String,
    pub capabilities: Arc<CapabilityStore>,
    pub broker_origin: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

/// A running proxy for one micro-app. Dropping this without calling `stop`
/// leaves the listener running (the accept loop only exits when the
/// shutdown signal fires or the receiver is dropped AND a connection
/// arrives) — always route through `MicroAppServer`/`MicroAppsState`'s own
/// stop path instead of letting this fall out of scope.
pub struct CspProxy {
    port: u16,
    upstream_host: String,
    upstream_port: u16,
    shutdown: Option<oneshot::Sender<()>>,
}

impl CspProxy {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The Vite host:port this proxy currently forwards to. Callers reusing
    /// a cached proxy (micro_apps_commands.rs's `micro_app_start`) need this
    /// to detect a stale pairing — e.g. Vite crashed and was respawned on a
    /// different port, but the proxy from before that respawn is still
    /// cached and would silently forward to whatever now occupies its old
    /// upstream port.
    pub fn upstream_port(&self) -> u16 {
        self.upstream_port
    }

    pub fn upstream_host(&self) -> &str {
        &self.upstream_host
    }

    /// Stop accepting new connections. In-flight requests/splices are left
    /// to finish on their own (matching how `stopMicroApp`'s plain
    /// `child.kill()` never waited for in-flight Vite requests either).
    pub fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// Start a proxy on an OS-assigned loopback port, forwarding to
/// `upstream_host:upstream_port` — Vite's own reported host, NOT hardcoded
/// to `127.0.0.1`: Vite (no `--host` flag) binds whatever "localhost"
/// resolves to on this machine, which on some CI runners is `::1`
/// (IPv6-only), not `127.0.0.1`. Connecting by name lets tokio's own
/// resolver match whatever Vite actually bound to, rather than assuming
/// IPv4. Injects/replaces the CSP header on normal responses and
/// transparently splices Vite's HMR WebSocket upgrade — the two behaviors
/// spike/tauri-csp-proxy/ (wayfinder ticket #22) validated on WebKitGTK
/// (against a `127.0.0.1`-bound upstream there, hence the literal address —
/// this generalizes it for the real, host-name-reported case).
pub async fn start(
    upstream_host: String,
    upstream_port: u16,
    deps: CspProxyDeps,
) -> Result<CspProxy, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("csp-proxy: failed to bind a listen port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();

    let state = Arc::new(ProxyState {
        self_origin: format!("http://127.0.0.1:{port}"),
        upstream_host: upstream_host.clone(),
        upstream_port,
        app_name: deps.app_name,
        capabilities: deps.capabilities,
        broker_origin: deps.broker_origin,
    });

    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let Ok((stream, peer)) = accepted else { continue };
                    let conn_state = state.clone();
                    tokio::spawn(async move {
                        let log_state = conn_state.clone();
                        let io = TokioIo::new(stream);
                        let service = service_fn(move |req| handle(req, peer, conn_state.clone()));
                        if let Err(err) = server_http1::Builder::new().serve_connection(io, service).with_upgrades().await {
                            eprintln!("[hearth] csp-proxy[{}][{peer}]: connection error: {err}", log_state.app_name);
                        }
                    });
                }
            }
        }
    });

    Ok(CspProxy {
        port,
        upstream_host,
        upstream_port,
        shutdown: Some(shutdown_tx),
    })
}

fn is_ws_upgrade(req: &Request<Incoming>) -> bool {
    let has_connection_upgrade = req
        .headers()
        .get(CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false);
    let has_upgrade_websocket = req
        .headers()
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    has_connection_upgrade && has_upgrade_websocket
}

fn bad_gateway(detail: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .body(Full::new(Bytes::from(format!("csp-proxy: {detail}"))))
        .expect("static bad-gateway response is well-formed")
}

async fn handle(
    mut req: Request<Incoming>,
    peer: SocketAddr,
    state: Arc<ProxyState>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let is_ws = is_ws_upgrade(&req);
    let path = req.uri().path().to_string();

    // Must be taken before `req` is handed to the upstream sender below —
    // this is the client-side half of the upgrade.
    let client_upgrade = is_ws.then(|| hyper::upgrade::on(&mut req));

    let upstream_stream =
        match TcpStream::connect((state.upstream_host.as_str(), state.upstream_port)).await {
            Ok(s) => s,
            Err(err) => {
                eprintln!(
                    "[hearth] csp-proxy[{}][{peer}]: upstream connect failed for {path}: {err}",
                    state.app_name
                );
                return Ok(bad_gateway("upstream connect failed"));
            }
        };
    let upstream_io = TokioIo::new(upstream_stream);
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(upstream_io).await {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!(
                "[hearth] csp-proxy[{}][{peer}]: upstream handshake failed for {path}: {err}",
                state.app_name
            );
            return Ok(bad_gateway("upstream handshake failed"));
        }
    };
    let conn_app_name = state.app_name.clone();
    tokio::spawn(async move {
        if let Err(err) = conn.with_upgrades().await {
            eprintln!(
                "[hearth] csp-proxy[{conn_app_name}][{peer}]: upstream connection error: {err}"
            );
        }
    });

    let mut upstream_resp = match sender.send_request(req).await {
        Ok(r) => r,
        Err(err) => {
            eprintln!(
                "[hearth] csp-proxy[{}][{peer}]: upstream request failed for {path}: {err}",
                state.app_name
            );
            return Ok(bad_gateway("upstream request failed"));
        }
    };

    if is_ws && upstream_resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        let upstream_upgrade = hyper::upgrade::on(&mut upstream_resp);
        let (parts, _empty_body) = upstream_resp.into_parts();
        let response = Response::from_parts(parts, Full::new(Bytes::new()));

        let app_name = state.app_name.clone();
        tokio::spawn(async move {
            let client_upgrade = client_upgrade.expect("is_ws implies client_upgrade is Some");
            match tokio::try_join!(client_upgrade, upstream_upgrade) {
                Ok((client_upgraded, upstream_upgraded)) => {
                    println!("[hearth] csp-proxy[{app_name}][{peer}]: SPLICE_START {path}");
                    let mut client_io = TokioIo::new(client_upgraded);
                    let mut upstream_io = TokioIo::new(upstream_upgraded);
                    match tokio::io::copy_bidirectional(&mut client_io, &mut upstream_io).await {
                        Ok((to_upstream, to_client)) => println!(
                            "[hearth] csp-proxy[{app_name}][{peer}]: SPLICE_END {path} client->upstream={to_upstream}B upstream->client={to_client}B"
                        ),
                        Err(err) => eprintln!("[hearth] csp-proxy[{app_name}][{peer}]: SPLICE_ERROR {path}: {err}"),
                    }
                }
                Err(err) => {
                    eprintln!("[hearth] csp-proxy[{app_name}][{peer}]: upgrade handshake failed for {path}: {err}")
                }
            }
        });

        return Ok(response);
    }

    // Read live off the capability store + broker-origin getter on every
    // response — a host approved (or the broker starting) while the app is
    // already open takes effect on its very next request, no restart
    // needed, same as session-policy.ts's own per-request read.
    let approved = state.capabilities.approved(&state.app_name);
    let broker = (state.broker_origin)();
    let csp = build_micro_app_csp(&state.self_origin, &approved, broker.as_deref());

    let (mut parts, body) = upstream_resp.into_parts();
    parts.headers.remove(CONTENT_SECURITY_POLICY);
    parts.headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&csp).expect("built CSP header value contains no invalid bytes"),
    );
    let bytes = body.collect().await?.to_bytes();
    Ok(Response::from_parts(parts, Full::new(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::micro_apps::capabilities::CapabilityStore;
    use http_body_util::Empty;
    use hyper::header::{HeaderName, HeaderValue};
    use std::convert::Infallible;
    use tokio::net::TcpListener as TestListener;

    #[test]
    fn build_micro_app_csp_an_ungranted_app_reaches_only_self_hmr_and_broker() {
        let csp = build_micro_app_csp("http://localhost:5183", &[], Some("http://127.0.0.1:49210"));
        let connect = csp
            .split("; ")
            .find(|d| d.starts_with("connect-src "))
            .unwrap();
        assert_eq!(
            connect,
            "connect-src 'self' ws://localhost:5183 http://127.0.0.1:49210"
        );
        assert!(!connect.contains("https://"));
    }

    #[test]
    fn build_micro_app_csp_a_granted_app_gets_exactly_its_approved_hosts_added() {
        let csp = build_micro_app_csp(
            "http://localhost:5183",
            &["https://www.googleapis.com".to_string()],
            Some("http://127.0.0.1:49210"),
        );
        let connect = csp
            .split("; ")
            .find(|d| d.starts_with("connect-src "))
            .unwrap();
        assert!(connect.contains("https://www.googleapis.com"));
        assert!(!connect.contains("https://evil.com"));
    }

    #[test]
    fn build_micro_app_csp_omits_the_broker_when_not_running() {
        let csp = build_micro_app_csp("http://localhost:5183", &[], None);
        let connect = csp
            .split("; ")
            .find(|d| d.starts_with("connect-src "))
            .unwrap();
        assert_eq!(connect, "connect-src 'self' ws://localhost:5183");
    }

    #[test]
    fn build_micro_app_csp_never_sets_a_connect_src_floor_looser_than_self() {
        let csp = build_micro_app_csp("http://localhost:5200", &[], None);
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("object-src 'none'"));
        assert!(csp.contains("base-uri 'self'"));
    }

    /// A minimal upstream: an hyper server that echoes a fixed body and sets
    /// its own (attacker-controllable, in the real threat model) CSP header
    /// — the proxy must strip and replace it, not merge or trust it.
    async fn spawn_fake_upstream(csp_to_strip: &'static str) -> u16 {
        let listener = TestListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |_req: Request<Incoming>| async move {
                        let mut resp =
                            Response::new(Full::new(Bytes::from_static(b"hello from upstream")));
                        resp.headers_mut().insert(
                            HeaderName::from_static("content-security-policy"),
                            HeaderValue::from_static(csp_to_strip),
                        );
                        Ok::<_, Infallible>(resp)
                    });
                    let _ = server_http1::Builder::new()
                        .serve_connection(io, service)
                        .await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn proxy_strips_upstream_csp_and_stamps_its_own() {
        let upstream_port = spawn_fake_upstream("default-src *; script-src 'unsafe-eval'").await;
        let capabilities = Arc::new(CapabilityStore::new(
            tempfile::tempdir().unwrap().path().join("caps.json"),
        ));
        let proxy = start(
            "127.0.0.1".to_string(),
            upstream_port,
            CspProxyDeps {
                app_name: "my-app".to_string(),
                capabilities: capabilities.clone(),
                broker_origin: Arc::new(|| None),
            },
        )
        .await
        .unwrap();
        let port = proxy.port();

        let client_stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(client_stream))
            .await
            .unwrap();
        tokio::spawn(conn);
        let req = Request::builder()
            .uri("/")
            .body(Empty::<Bytes>::new())
            .unwrap();
        let resp = sender.send_request(req).await.unwrap();

        let csp = resp
            .headers()
            .get(CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains(&format!("connect-src 'self' ws://127.0.0.1:{port}")));
        assert!(!csp.contains("unsafe-eval"));
        assert!(!csp.contains("default-src *"));

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"hello from upstream");

        proxy.stop();
    }

    #[tokio::test]
    async fn proxy_reflects_a_newly_approved_host_without_restarting() {
        let upstream_port = spawn_fake_upstream("").await;
        let caps_path = tempfile::tempdir().unwrap().path().join("caps.json");
        let capabilities = Arc::new(CapabilityStore::new(caps_path));
        let proxy = start(
            "127.0.0.1".to_string(),
            upstream_port,
            CspProxyDeps {
                app_name: "my-app".to_string(),
                capabilities: capabilities.clone(),
                broker_origin: Arc::new(|| None),
            },
        )
        .await
        .unwrap();
        let port = proxy.port();

        let fetch_csp = || async {
            let client_stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let (mut sender, conn) =
                hyper::client::conn::http1::handshake(TokioIo::new(client_stream))
                    .await
                    .unwrap();
            tokio::spawn(conn);
            let req = Request::builder()
                .uri("/")
                .body(Empty::<Bytes>::new())
                .unwrap();
            let resp = sender.send_request(req).await.unwrap();
            resp.headers()
                .get(CONTENT_SECURITY_POLICY)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        };

        let before = fetch_csp().await;
        assert!(!before.contains("api.example.com"));

        capabilities
            .approve("my-app", &["https://api.example.com".to_string()])
            .unwrap();

        let after = fetch_csp().await;
        assert!(after.contains("https://api.example.com"));

        proxy.stop();
    }
}
