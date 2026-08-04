// Main-process client for the renderer overlay plugin's dev endpoint (W1). Posts
// pin/apply/release to the Vite dev server (a separate process), best-effort: the
// overlay is a dev-only optimization, so a failed POST (production build, server
// not up) is swallowed — correctness still comes from the git-HEAD path + commits.
//
// Ported from electron/main/self-mod/overlay-client.ts. That version's methods
// are async (`Promise<void>`) around a fetch that's fired and (usually) not
// awaited by callers; `ureq` is a synchronous HTTP client, so this port's
// methods block for the (short-timeout) duration of the POST instead. Since the
// call target is always localhost and every error is swallowed either way, this
// doesn't change observable behavior for the caller — only whether the wait
// happens on this thread or in the JS event loop.

use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;

const ENDPOINT: &str = "/__hearth/self-mod";

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(2))
            .build()
    })
}

/// TS builds the POST target with `new URL(ENDPOINT, devUrl)` — an absolute-path
/// second argument, so the result keeps only `devUrl`'s origin and discards its
/// path/query. Rather than pull in a full URL-parsing crate for that one
/// behavior, hand-roll it: keep the scheme + authority, drop everything from the
/// first `/` after it.
fn origin(dev_url: &str) -> Option<String> {
    let scheme_end = dev_url.find("://")? + 3;
    let rest = &dev_url[scheme_end..];
    let authority_end = rest.find('/').unwrap_or(rest.len());
    Some(format!(
        "{}{}",
        &dev_url[..scheme_end],
        &rest[..authority_end]
    ))
}

fn post(dev_url: Option<&str>, body: &Value) {
    let Some(dev_url) = dev_url else { return };
    let Some(origin) = origin(dev_url) else {
        return;
    };
    let url = format!("{origin}{ENDPOINT}");
    // Best-effort: overlay is a dev-only optimization.
    let _ = agent().post(&url).send_json(body.clone());
}

/// `get_dev_url` mirrors the TS factory's `getDevUrl: () => string | null`
/// closure — the caller supplies how to look up the current dev server URL
/// (None when not running under Vite, e.g. a production build).
pub struct OverlayClient<F: Fn() -> Option<String>> {
    get_dev_url: F,
}

impl<F: Fn() -> Option<String>> OverlayClient<F> {
    pub fn new(get_dev_url: F) -> Self {
        Self { get_dev_url }
    }

    pub fn pin(&self, repo_rel_path: &str, baseline: &str) {
        post(
            (self.get_dev_url)().as_deref(),
            &serde_json::json!({ "op": "pin", "path": repo_rel_path, "baseline": baseline }),
        );
    }

    pub fn apply(&self, repo_rel_paths: &[String]) {
        post(
            (self.get_dev_url)().as_deref(),
            &serde_json::json!({ "op": "apply", "paths": repo_rel_paths }),
        );
    }

    pub fn release(&self, repo_rel_paths: &[String]) {
        post(
            (self.get_dev_url)().as_deref(),
            &serde_json::json!({ "op": "release", "paths": repo_rel_paths }),
        );
    }

    /// Mark a self-mod turn active/inactive. While active, the overlay plugin
    /// suppresses Vite's autonomous full-reload for full-reload-tier files so a
    /// structural edit can be applied under the morph cover at turn end (B6).
    pub fn turn_start(&self) {
        post(
            (self.get_dev_url)().as_deref(),
            &serde_json::json!({ "op": "turn-start" }),
        );
    }

    pub fn turn_end(&self) {
        post(
            (self.get_dev_url)().as_deref(),
            &serde_json::json!({ "op": "turn-end" }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration as StdDuration;

    /// Spins up a one-shot local HTTP server: accepts a single connection,
    /// reads the request, sends back `200 OK`, and hands the request line +
    /// body back to the test over a channel. Exercises the real client against
    /// a real socket instead of mocking the HTTP layer.
    ///
    /// A single `read()` call can return before the whole request has arrived
    /// (headers and body can land in separate TCP segments) — so this keeps
    /// reading until it has seen the header terminator plus a full
    /// Content-Length body, rather than trusting the first read to have it all.
    fn one_shot_server() -> (String, mpsc::Receiver<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(StdDuration::from_secs(2)))
                    .ok();
                let mut raw: Vec<u8> = Vec::new();
                let mut buf = [0u8; 4096];
                let mut header_end: Option<usize> = None;
                let mut content_length: usize = 0;
                loop {
                    if let Some(he) = header_end {
                        if raw.len() >= he + content_length {
                            break;
                        }
                    }
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => raw.extend_from_slice(&buf[..n]),
                    }
                    if header_end.is_none() {
                        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let header_text = String::from_utf8_lossy(&raw[..pos]).to_string();
                            for line in header_text.split("\r\n") {
                                if let Some(v) =
                                    line.to_ascii_lowercase().strip_prefix("content-length:")
                                {
                                    content_length = v.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                }
                let full = String::from_utf8_lossy(&raw).to_string();
                let request_line = full.split("\r\n").next().unwrap_or("").to_string();
                let body = header_end
                    .map(|he| String::from_utf8_lossy(&raw[he..]).to_string())
                    .unwrap_or_default();
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = tx.send((request_line, body));
            }
        });
        (format!("http://{addr}"), rx)
    }

    #[test]
    fn origin_strips_path_and_query() {
        assert_eq!(
            origin("http://localhost:5173/some/path?x=1").as_deref(),
            Some("http://localhost:5173")
        );
        assert_eq!(
            origin("http://localhost:5173").as_deref(),
            Some("http://localhost:5173")
        );
        assert_eq!(origin("not-a-url"), None);
    }

    #[test]
    fn pin_posts_to_the_self_mod_endpoint() {
        let (dev_url, rx) = one_shot_server();
        let client = OverlayClient::new(|| Some(dev_url.clone()));
        client.pin("src/a.ts", "OLD");
        let (request_line, body) = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        assert!(
            request_line.starts_with("POST /__hearth/self-mod"),
            "{request_line}"
        );
        let parsed: Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(parsed["op"], "pin");
        assert_eq!(parsed["path"], "src/a.ts");
        assert_eq!(parsed["baseline"], "OLD");
    }

    #[test]
    fn apply_posts_the_path_list() {
        let (dev_url, rx) = one_shot_server();
        let client = OverlayClient::new(|| Some(dev_url.clone()));
        client.apply(&["src/a.ts".to_string(), "src/b.ts".to_string()]);
        let (_, body) = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        let parsed: Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(parsed["op"], "apply");
        assert_eq!(parsed["paths"][0], "src/a.ts");
        assert_eq!(parsed["paths"][1], "src/b.ts");
    }

    #[test]
    fn release_posts_the_path_list() {
        let (dev_url, rx) = one_shot_server();
        let client = OverlayClient::new(|| Some(dev_url.clone()));
        client.release(&["src/a.ts".to_string()]);
        let (_, body) = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        let parsed: Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(parsed["op"], "release");
        assert_eq!(parsed["paths"][0], "src/a.ts");
    }

    #[test]
    fn turn_start_and_turn_end_post_op_only() {
        let (dev_url, rx) = one_shot_server();
        let client = OverlayClient::new(|| Some(dev_url.clone()));
        client.turn_start();
        let (_, body) = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        let parsed: Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(parsed["op"], "turn-start");

        let (dev_url, rx) = one_shot_server();
        let client = OverlayClient::new(|| Some(dev_url.clone()));
        client.turn_end();
        let (_, body) = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        let parsed: Value = serde_json::from_str(&body).expect("json body");
        assert_eq!(parsed["op"], "turn-end");
    }

    #[test]
    fn no_dev_url_is_a_silent_noop() {
        let client = OverlayClient::new(|| None);
        // Must not panic or hang even though there is no server to talk to.
        client.pin("src/a.ts", "OLD");
        client.turn_end();
    }

    #[test]
    fn unreachable_dev_url_is_swallowed() {
        // Nothing listens on this loopback port — the POST fails and the error
        // must be swallowed, matching the TS `catch {}` semantics.
        let client = OverlayClient::new(|| Some("http://127.0.0.1:1".to_string()));
        client.pin("src/a.ts", "OLD");
    }
}
