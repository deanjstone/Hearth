// Spike for wayfinder ticket #22 (deanjstone/Hearth): does a Rust-owned
// reverse proxy in front of a real Vite dev server (a) inject/replace the
// Content-Security-Policy header on normal HTTP responses, and (b)
// transparently pass through the WebSocket upgrade Vite's HMR client needs,
// on the same real-loopback-HTTP origin shape the HMR spike
// (../../tauri-hmr-check/, docs/decisions/rust-tauri-feasibility.md §12)
// already validated? See #20's locked design this prototypes.
//
// Deliberately the smallest thing that answers the question: one upstream,
// no TLS, no config file, a fresh upstream connection per client connection
// (no pooling). Every WebSocket upgrade this proxy actually splices prints a
// SPLICE_START/SPLICE_END line — that's the positive, proxy-side evidence
// that HMR traffic transited the proxy, not Vite's direct-fallback path
// (Vite's client only takes that fallback if the first, proxied, attempt
// fails — see docs/config/server-options.html#server-hmr — so a passing
// round-trip could otherwise be explained by the proxy being silently
// bypassed rather than by it actually working).

use std::net::SocketAddr;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderValue, CONNECTION, CONTENT_SECURITY_POLICY, UPGRADE};
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};

const LISTEN: &str = "127.0.0.1:5199";
const UPSTREAM: &str = "127.0.0.1:5183";
const INJECTED_CSP: &str = "default-src * data: blob: 'unsafe-inline' 'unsafe-eval'; frame-ancestors 'self'";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(LISTEN).await?;
    println!("csp-proxy: listening on http://{LISTEN}, forwarding to http://{UPSTREAM}");

    loop {
        let (stream, peer) = listener.accept().await?;
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req| handle(req, peer));
            if let Err(err) = server_http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                eprintln!("csp-proxy[{peer}]: connection error: {err}");
            }
        });
    }
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
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let is_ws = is_ws_upgrade(&req);
    let path = req.uri().path().to_string();

    // Must be taken before `req` is handed to the upstream sender below —
    // this is the client-side half of the upgrade (the OnUpgrade future
    // hyper stashed in the request's extensions when it saw Connection:
    // Upgrade come off the wire).
    let client_upgrade = is_ws.then(|| hyper::upgrade::on(&mut req));

    let upstream_stream = match TcpStream::connect(UPSTREAM).await {
        Ok(s) => s,
        Err(err) => {
            eprintln!("csp-proxy[{peer}]: upstream connect failed for {path}: {err}");
            return Ok(bad_gateway("upstream connect failed"));
        }
    };
    let upstream_io = TokioIo::new(upstream_stream);
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(upstream_io).await {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("csp-proxy[{peer}]: upstream handshake failed for {path}: {err}");
            return Ok(bad_gateway("upstream handshake failed"));
        }
    };
    tokio::spawn(async move {
        if let Err(err) = conn.with_upgrades().await {
            eprintln!("csp-proxy[{peer}]: upstream connection error: {err}");
        }
    });

    let mut upstream_resp = match sender.send_request(req).await {
        Ok(r) => r,
        Err(err) => {
            eprintln!("csp-proxy[{peer}]: upstream request failed for {path}: {err}");
            return Ok(bad_gateway("upstream request failed"));
        }
    };

    if is_ws && upstream_resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        let upstream_upgrade = hyper::upgrade::on(&mut upstream_resp);
        let (parts, _empty_body) = upstream_resp.into_parts();
        let response = Response::from_parts(parts, Full::new(Bytes::new()));

        tokio::spawn(async move {
            let client_upgrade = client_upgrade.expect("is_ws implies client_upgrade is Some");
            match tokio::try_join!(client_upgrade, upstream_upgrade) {
                Ok((client_upgraded, upstream_upgraded)) => {
                    println!("csp-proxy[{peer}]: SPLICE_START {path}");
                    let mut client_io = TokioIo::new(client_upgraded);
                    let mut upstream_io = TokioIo::new(upstream_upgraded);
                    match tokio::io::copy_bidirectional(&mut client_io, &mut upstream_io).await {
                        Ok((to_upstream, to_client)) => println!(
                            "csp-proxy[{peer}]: SPLICE_END {path} client->upstream={to_upstream}B upstream->client={to_client}B"
                        ),
                        Err(err) => eprintln!("csp-proxy[{peer}]: SPLICE_ERROR {path}: {err}"),
                    }
                }
                Err(err) => {
                    eprintln!("csp-proxy[{peer}]: upgrade handshake failed for {path}: {err}")
                }
            }
        });

        return Ok(response);
    }

    let (mut parts, body) = upstream_resp.into_parts();
    parts.headers.remove(CONTENT_SECURITY_POLICY);
    parts
        .headers
        .insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(INJECTED_CSP));
    let bytes = body.collect().await?.to_bytes();
    Ok(Response::from_parts(parts, Full::new(bytes)))
}
