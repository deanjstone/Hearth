// The agent's bridge to the LIVE app: it can both see it and drive it.
//
// Rust port of electron/main/agent-bridge.ts's `/snapshot` + `/eval` endpoints
// (Hearth#27 Phase 2). The renderer only gets `window.hearth` inside the real
// app, so the only process that can capture or script it is this one. A tiny
// loopback HTTP server (base URL written to .hearth/bridge-url, same file
// electron/main/agent-bridge.ts wrote) exposes:
//
//   GET  /snapshot[?path=/route]  -> PNG of the live window (or a route,
//                                    captured in a visible-but-offscreen
//                                    window so the user's view is untouched —
//                                    see snapshot_window's doc comment for why
//                                    a truly hidden window doesn't work on
//                                    WebKitGTK, per spike/tauri-hidden-capture).
//   POST /eval   {code}           -> runs JS in the live renderer via
//                                    eval_with_callback and returns the
//                                    (JSON-serializable) result. This is how
//                                    the agent clicks/fills/reads/navigates —
//                                    anything the user could, including
//                                    window.hearth IPC.
//
// scripts/view-app.mjs and electron/main/agent-tools/hearth-mcp-server.mjs
// (both unchanged JS) hit this exactly as they hit the Electron version —
// same URL/token file paths, same request/response shapes.
//
// The embedded persistent browser (`/browser/*` on the Electron side,
// BrowserManager-backed) is NOT ported here — it isn't named anywhere in
// spec #26's user stories/decisions, only the shell's own `view_app`/
// `read_ui`/`click`/`fill`/`eval_js` loop is in MVP scope.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Wry};
use tiny_http::{Header, Method, Request, Response, Server};

const OFFSCREEN_LABEL: &str = "hearth-snapshot";
const TOKEN_HEADER: &str = "x-hearth-token";
const EVAL_TIMEOUT: Duration = Duration::from_secs(15);
const WINDOW_BUILD_TIMEOUT: Duration = Duration::from_secs(10);
// Let the route render + paint (memory history, not URL-driven) — same delay
// electron/main/agent-bridge.ts uses after sending its viewNavigate IPC.
const ROUTE_RENDER_DELAY: Duration = Duration::from_millis(500);

pub fn start(app: AppHandle<Wry>, repo_root: PathBuf) {
    let token = generate_token();
    let server = match Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[hearth] bridge: failed to bind loopback server: {e}");
            return;
        }
    };
    let port = match server.server_addr().to_ip() {
        Some(addr) => addr.port(),
        None => {
            eprintln!("[hearth] bridge: loopback server has no IP address");
            return;
        }
    };
    if let Err(e) = write_bridge_files(&repo_root, port, &token) {
        eprintln!("[hearth] bridge: failed to write .hearth/bridge-* files: {e}");
    }

    let offscreen: Arc<Mutex<Option<WebviewWindow<Wry>>>> = Arc::new(Mutex::new(None));

    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            handle_request(request, &app, port, &token, &offscreen);
        }
    });
}

// 32 random bytes straight from /dev/urandom (standard on every Linux target
// this port supports — Linux-only, per spec #26's scope decision), hex
// encoded. Avoids pulling in the `rand` crate for one call site.
fn generate_token() -> String {
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("bridge: failed to read /dev/urandom for the bearer token");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_bridge_files(repo_root: &Path, port: u16, token: &str) -> std::io::Result<()> {
    let dir = repo_root.join(".hearth");
    std::fs::create_dir_all(&dir)?;
    let url_file = dir.join("bridge-url");
    let token_file = dir.join("bridge-token");
    std::fs::write(&url_file, format!("http://127.0.0.1:{port}\n"))?;
    std::fs::write(&token_file, format!("{token}\n"))?;
    // Restrict to the owner: the token is the only thing gating renderer RCE.
    // Best-effort, like agent-bridge.ts's chmodSync try/catch (e.g. on
    // filesystems that don't support permission bits).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&url_file, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn header_value(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

/// Pure auth/DNS-rebinding check, split out from request handling so it's
/// unit-testable without a real tiny_http server — see #[cfg(test)] below.
/// Mirrors agent-bridge.ts's two checks: (1) DNS-rebinding defense (U8) — a
/// browser script on attacker.example whose DNS flips to 127.0.0.1 reaches
/// this port, but carries a Host header naming the attacker's origin and an
/// Origin header; serve only requests addressed to exactly this bound
/// loopback address with no Origin (our MCP child / scripts send neither).
/// (2) the per-boot bearer token, checked after the Host/Origin gate.
fn check_auth(
    host: Option<&str>,
    origin: Option<&str>,
    provided_token: Option<&str>,
    expected_host: &str,
    expected_token: &str,
) -> Result<(), (u16, &'static str)> {
    if host != Some(expected_host) || origin.is_some() {
        return Err((403, "forbidden"));
    }
    if provided_token != Some(expected_token) {
        return Err((401, "unauthorized"));
    }
    Ok(())
}

fn split_query(url: &str) -> (&str, Option<&str>) {
    match url.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (url, None),
    }
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            return Some(urlencoding_decode(v));
        }
    }
    None
}

// Minimal percent-decoder (path params here are simple route strings like
// "/history", not general form data) — avoids pulling in a URL-parsing crate
// for one query param.
fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond_text(request: Request, status: u16, body: &str) {
    let _ = request.respond(Response::from_string(body).with_status_code(status));
}

fn respond_png(request: Request, png: Vec<u8>) {
    let header = Header::from_bytes(&b"Content-Type"[..], &b"image/png"[..]).unwrap();
    let _ = request.respond(Response::from_data(png).with_header(header));
}

fn respond_json(request: Request, status: u16, value: &serde_json::Value) {
    let body = value.to_string();
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
    let _ = request.respond(
        Response::from_string(body)
            .with_header(header)
            .with_status_code(status),
    );
}

fn handle_request(
    mut request: Request,
    app: &AppHandle<Wry>,
    port: u16,
    token: &str,
    offscreen: &Arc<Mutex<Option<WebviewWindow<Wry>>>>,
) {
    let expected_host = format!("127.0.0.1:{port}");
    let host = header_value(&request, "Host");
    let origin = header_value(&request, "Origin");
    let provided_token = header_value(&request, TOKEN_HEADER);
    if let Err((status, msg)) = check_auth(
        host.as_deref(),
        origin.as_deref(),
        provided_token.as_deref(),
        &expected_host,
        token,
    ) {
        respond_text(request, status, msg);
        return;
    }

    let url = request.url().to_string();
    let (path, query) = split_query(&url);

    match (request.method(), path) {
        (Method::Get, "/snapshot") => {
            let route = query_param(query, "path");
            match capture_snapshot(app, offscreen, route.as_deref()) {
                Ok(png) => respond_png(request, png),
                Err(e) => respond_text(request, 500, &e),
            }
        }
        (Method::Post, "/eval") => {
            let mut body = String::new();
            if request.as_reader().read_to_string(&mut body).is_err() {
                respond_json(
                    request,
                    400,
                    &serde_json::json!({ "ok": false, "error": "failed to read request body" }),
                );
                return;
            }
            let code = match serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string))
            {
                Some(code) => code,
                None => {
                    respond_json(
                        request,
                        400,
                        &serde_json::json!({ "ok": false, "error": "expected { code: string }" }),
                    );
                    return;
                }
            };
            // /eval errors are returned as ok:false (HTTP 200) so the agent
            // sees them, matching agent-bridge.ts's error-handling shape.
            match eval_in_app(app, &code) {
                Ok(result) => respond_json(
                    request,
                    200,
                    &serde_json::json!({ "ok": true, "result": result }),
                ),
                Err(e) => respond_json(
                    request,
                    200,
                    &serde_json::json!({ "ok": false, "error": e }),
                ),
            }
        }
        _ => respond_text(request, 404, "not found"),
    }
}

fn capture_snapshot(
    app: &AppHandle<Wry>,
    offscreen: &Arc<Mutex<Option<WebviewWindow<Wry>>>>,
    route: Option<&str>,
) -> Result<Vec<u8>, String> {
    let window = match route {
        None => app
            .get_webview_window("main")
            .ok_or_else(|| "the \"main\" window is not available".to_string())?,
        Some(path) => {
            let window = ensure_offscreen(app, offscreen)?;
            app.emit_to(
                OFFSCREEN_LABEL,
                "view:navigate",
                serde_json::json!({ "path": path }),
            )
            .map_err(|e| e.to_string())?;
            std::thread::sleep(ROUTE_RENDER_DELAY);
            window
        }
    };
    snapshot_window(&window)
}

// One reusable offscreen window for route captures, created lazily on first
// use and kept alive for the bridge's lifetime — mirrors agent-bridge.ts's
// `ensureOffscreen`. It boots the full app (same dev-server URL as the main
// window) so the "view:navigate" listener registered in src/main.tsx can
// route it, exactly like the main window would.
fn ensure_offscreen(
    app: &AppHandle<Wry>,
    offscreen: &Arc<Mutex<Option<WebviewWindow<Wry>>>>,
) -> Result<WebviewWindow<Wry>, String> {
    if let Some(w) = offscreen.lock().unwrap().as_ref() {
        return Ok(w.clone());
    }

    let dev_url = app
        .config()
        .build
        .dev_url
        .clone()
        .ok_or_else(|| "no build.devUrl configured".to_string())?;

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let app_for_main_thread = app.clone();
    let offscreen_for_main_thread = offscreen.clone();
    app.run_on_main_thread(move || {
        let result = WebviewWindowBuilder::new(
            &app_for_main_thread,
            OFFSCREEN_LABEL,
            WebviewUrl::External(dev_url),
        )
        .inner_size(1280.0, 800.0)
        // Visible-but-off-screen, not `.visible(false)`: WebKitGTK never
        // realizes a genuinely hidden window's compositor surface, so a
        // snapshot call on it fails outright — confirmed in
        // spike/tauri-hidden-capture/ (wayfinder ticket #17). Positioned
        // far outside any real display so nothing appears on the user's
        // screen; undecorated + skip-taskbar + unfocused so it doesn't
        // steal focus or show up in the taskbar/alt-tab either.
        .position(-32000.0, -32000.0)
        .decorations(false)
        .skip_taskbar(true)
        .focused(false)
        .visible(true)
        .build();
        let sent = match result {
            Ok(window) => {
                *offscreen_for_main_thread.lock().unwrap() = Some(window);
                tx.send(Ok(()))
            }
            Err(e) => tx.send(Err(e.to_string())),
        };
        let _ = sent;
    })
    .map_err(|e| e.to_string())?;

    rx.recv_timeout(WINDOW_BUILD_TIMEOUT)
        .map_err(|_| "timed out creating the offscreen snapshot window".to_string())??;

    let window = offscreen
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "offscreen window vanished after creation".to_string())?;
    Ok(window)
}

#[cfg(target_os = "linux")]
fn snapshot_window(window: &WebviewWindow<Wry>) -> Result<Vec<u8>, String> {
    use webkit2gtk::{gio, SnapshotOptions, SnapshotRegion, WebViewExt};

    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, String>>();
    window
        .with_webview(move |webview| {
            webview.inner().snapshot(
                SnapshotRegion::Visible,
                SnapshotOptions::empty(),
                gio::Cancellable::NONE,
                move |result| {
                    let outcome = match result {
                        Ok(surface) => match cairo::ImageSurface::try_from(surface) {
                            Ok(image) => {
                                let mut buf = Vec::new();
                                image
                                    .write_to_png(&mut buf)
                                    .map(|()| buf)
                                    .map_err(|e| format!("PNG encode error: {e}"))
                            }
                            Err(_) => Err("snapshot surface was not an ImageSurface".to_string()),
                        },
                        Err(e) => Err(format!("snapshot error: {e}")),
                    };
                    let _ = tx.send(outcome);
                },
            );
        })
        .map_err(|e| e.to_string())?;

    rx.recv_timeout(WINDOW_BUILD_TIMEOUT)
        .map_err(|_| "timed out waiting for the snapshot".to_string())?
}

#[cfg(not(target_os = "linux"))]
fn snapshot_window(_window: &WebviewWindow<Wry>) -> Result<Vec<u8>, String> {
    Err("snapshotting is only implemented for WebKitGTK (Linux)".to_string())
}

// Wraps the caller's expression in a try/catch so a thrown exception comes
// back as a structured `{__hearth_ok:false, __hearth_error}` payload instead
// of silently vanishing: eval_with_callback's docs say "exception is ignored"
// (a Windows-only wry limitation per its doc comment), but the underlying
// WebKitGTK path also collapses a `run_javascript` error to an empty string
// (see wry's webkitgtk eval: `.unwrap_or_default()` on both the outer Result
// and the inner Option) — so without this wrapper we can't tell a thrown
// exception apart from a legitimately empty/undefined result.
fn wrap_eval_code(code: &str) -> String {
    format!(
        "(function(){{ try {{ return JSON.stringify({{ __hearth_ok: true, __hearth_value: ({code}) }}); }} \
         catch (e) {{ return JSON.stringify({{ __hearth_ok: false, __hearth_error: (e && e.message) || String(e) }}); }} }})()"
    )
}

fn eval_in_app(app: &AppHandle<Wry>, code: &str) -> Result<serde_json::Value, String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "the \"main\" window is not available".to_string())?;

    let (tx, rx) = mpsc::channel::<String>();
    window
        .eval_with_callback(wrap_eval_code(code), move |result| {
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;

    let raw = rx
        .recv_timeout(EVAL_TIMEOUT)
        .map_err(|_| "timed out evaluating JS in the live app".to_string())?;

    // `raw` is itself a JSON-encoded string (eval_with_callback serializes
    // the JS return value, and our wrapper above returns a JSON.stringify'd
    // string) — decode twice. An empty/unparseable outer string means wry
    // swallowed a real error (see wrap_eval_code's comment) rather than the
    // page legitimately returning it.
    let outer: String = serde_json::from_str(&raw)
        .map_err(|_| "eval produced no result (a native error was likely swallowed)".to_string())?;
    let inner: serde_json::Value =
        serde_json::from_str(&outer).map_err(|e| format!("eval result was not valid JSON: {e}"))?;

    if inner.get("__hearth_ok").and_then(|v| v.as_bool()) == Some(true) {
        Ok(inner
            .get("__hearth_value")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    } else {
        Err(inner
            .get("__hearth_error")
            .and_then(|v| v.as_str())
            .unwrap_or("eval failed")
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_mismatched_host() {
        let result = check_auth(
            Some("evil.example:1234"),
            None,
            Some("tok"),
            "127.0.0.1:1234",
            "tok",
        );
        assert_eq!(result, Err((403, "forbidden")));
    }

    #[test]
    fn rejects_any_origin_header() {
        // A real browser page always sends Origin; our MCP child / scripts
        // never do — presence alone is the DNS-rebinding tell.
        let result = check_auth(
            Some("127.0.0.1:1234"),
            Some("http://127.0.0.1:1234"),
            Some("tok"),
            "127.0.0.1:1234",
            "tok",
        );
        assert_eq!(result, Err((403, "forbidden")));
    }

    #[test]
    fn rejects_missing_host() {
        let result = check_auth(None, None, Some("tok"), "127.0.0.1:1234", "tok");
        assert_eq!(result, Err((403, "forbidden")));
    }

    #[test]
    fn rejects_wrong_token() {
        let result = check_auth(
            Some("127.0.0.1:1234"),
            None,
            Some("wrong"),
            "127.0.0.1:1234",
            "tok",
        );
        assert_eq!(result, Err((401, "unauthorized")));
    }

    #[test]
    fn rejects_missing_token() {
        let result = check_auth(Some("127.0.0.1:1234"), None, None, "127.0.0.1:1234", "tok");
        assert_eq!(result, Err((401, "unauthorized")));
    }

    #[test]
    fn accepts_matching_host_no_origin_and_right_token() {
        let result = check_auth(
            Some("127.0.0.1:1234"),
            None,
            Some("tok"),
            "127.0.0.1:1234",
            "tok",
        );
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn splits_query_string() {
        assert_eq!(split_query("/snapshot"), ("/snapshot", None));
        assert_eq!(
            split_query("/snapshot?path=%2Fhistory"),
            ("/snapshot", Some("path=%2Fhistory"))
        );
    }

    #[test]
    fn decodes_query_param() {
        assert_eq!(
            query_param(Some("path=%2Fhistory"), "path"),
            Some("/history".to_string())
        );
        assert_eq!(query_param(Some("foo=bar"), "path"), None);
        assert_eq!(query_param(None, "path"), None);
    }

    #[test]
    fn wraps_eval_code_as_try_catch_expression() {
        let wrapped = wrap_eval_code("1 + 1");
        assert!(wrapped.contains("__hearth_ok"));
        assert!(wrapped.contains("(1 + 1)"));
    }
}
