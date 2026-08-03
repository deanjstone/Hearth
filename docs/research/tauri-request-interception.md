# Tauri/WebKitGTK request interception & CSP-injection capability survey

**Status:** research only. Answers GitHub issue [#19](https://github.com/deanjstone/Hearth/issues/19),
feeding §8d of `docs/decisions/rust-tauri-feasibility.md`.

**Versions pinned by this repo's spike** (`spike/tauri-hmr-check/src-tauri/Cargo.lock`,
confirmed by direct read, not assumed from the feasibility doc):
`tauri = 2.11.5`, `wry = 0.55.1`, `webkit2gtk = 2.0.2` (the `webkit2gtk-rs` crate,
tag `webkit2gtk-rs-v2.0.2`, wrapping WebKitGTK's C API).

**Verdict up front:** both capabilities are achievable, but neither is a drop-in
equivalent to Electron's `session` APIs — both require moving *where* the
control is enforced, not finding a like-for-like Tauri method:

1. **CSP header injection** — no Tauri/wry/WebKitGTK hook exists for modifying
   response headers on real `http(s)://` network requests (only on Tauri's own
   custom URI-scheme protocol, which Hearth doesn't use for shell/micro-app
   content and structurally can't switch to without risking the just-resolved
   HMR-on-WebKitGTK finding — see below). The workable design is a small
   Rust-owned reverse proxy in front of the shell/micro-app dev servers,
   which is not a webview API at all and sidesteps the gap rather than
   closing it.
2. **Permission gating** — Tauri's ACL/capability system is the wrong tool
   (it gates IPC *commands*, not browser device permissions). The real
   equivalent is WebKitGTK's native `WebKitWebView::permission-request`
   signal, reachable from Rust via the `webkit2gtk` crate and Tauri's
   `with_webview` escape hatch. This one expresses "deny by default, no
   exceptions" *more* simply than Electron's callback API.

Both are source-verified (method signatures read from the actual pinned crate
versions / their tagged source, not general knowledge) but **neither has been
prototyped**. Unlike §12's HMR finding, this ticket is desk research; a
narrow follow-up spike is recommended before either design is relied on (see
"Recommended next spikes" at the end).

---

## 1. Response-header interception (CSP injection)

### What Hearth needs

`electron/main/micro-apps/session-policy.ts` intercepts `onHeadersReceived`
session-wide and, for any response whose origin matches the shell or a known
micro-app, deletes any upstream `Content-Security-Policy` header and replaces
it with one computed from `buildShellCsp()` / `buildMicroAppCsp()` — the
latter is **dynamic at runtime**: it depends on which hosts the user has
granted via `CapabilityStore`, so it can't be baked in at build time.

Critically, per the file's own comments and `microAppForOrigin()` in
`electron/main/micro-apps/server.ts` (`URL_RE` matches
`https?://(localhost|127.0.0.1):\d+`), **both the shell and every micro-app
are served over real loopback HTTP** (a Vite dev server), not a custom
`app://`-style scheme — "in BOTH dev and packaged self-evolving builds" per
the code comment. This is not incidental: §12 of the feasibility doc spiked
and confirmed that Vite's HMR self-mod trick survives WebKitGTK specifically
*because* the webview navigates to the real dev-server URL unmodified. Any
design for CSP injection that changes what origin/scheme the page loads from
reopens that already-closed risk.

### What Tauri/wry/WebKitGTK actually offer

| Mechanism | Scope | Verdict |
|---|---|---|
| `tauri::Builder::register_uri_scheme_protocol` / `register_asynchronous_uri_scheme_protocol` ([docs.rs/tauri/2.11.5](https://docs.rs/tauri/2.11.5/tauri/struct.Builder.html)) | Custom URI schemes only (e.g. `app://`). Doc text: "Leverages `setURLSchemeHandler` on macOS, `AddWebResourceRequestedFilter` on Windows and `webkit-web-context-register-uri-scheme` on Linux." Handler builds the *entire* `http::Response` (status + headers + body) from scratch. | Full header control, but **only for a scheme you register** — not for real `http://localhost:PORT` traffic. |
| `WebviewBuilder::on_web_resource_request` ([docs.rs/tauri/2.11.5/tauri/webview/struct.WebviewBuilder.html](https://docs.rs/tauri/2.11.5/tauri/webview/struct.WebviewBuilder.html)) | Doc text, quoted verbatim: **"Currently only implemented for the `tauri` URI protocol."** and **"this is not executed for the [dev server / external URL] case"** (paraphrased from the doc's explicit dev-server carve-out). Example in the docs shows mutating `Content-Security-Policy` via `response.headers_mut()` — exactly the shape Hearth wants, but gated to the wrong protocol. | Closest API match by shape, wrong by scope — doesn't fire for Hearth's actual dev-server-served content. |
| `wry::WebViewBuilder::with_custom_protocol` / `with_asynchronous_custom_protocol` ([docs.rs/wry/0.55.1](https://docs.rs/wry/0.55.1/wry/struct.WebViewBuilder.html)) | Same custom-scheme-only shape as Tauri's wrapper above (Tauri's version is a thin wrapper over this). | Same limitation. |
| `wry::WebViewBuilder::with_headers` / `with_url_and_headers` | Sets **request** headers Wry sends when it *loads* the initial URL — one-shot, outbound, not a response interceptor, and not per-subsequent-request. | Not applicable — this is for the initial navigation only, not ongoing resource responses. |
| `tauri-plugin-http` ([v2.tauri.app/plugin/http-client](https://v2.tauri.app/plugin/http-client/)) | A Rust-backed HTTP client the *page's own JS* must explicitly `import { fetch } from '@tauri-apps/plugin-http'` and call — opt-in, not a transparent proxy over the webview's native `fetch`/`XHR`/`WebSocket`. Confirmed via the plugin's own docs example and its ACL permission requirements (`allow-fetch` + capability-scoped URLs). | Irrelevant to this problem — doesn't touch traffic the page makes through its normal browser APIs. |
| `app.security.csp` in `tauri.conf.json` ([v2.tauri.app/reference/config](https://v2.tauri.app/reference/config/)) | Static string injected into HTML "on all HTML files on the built application," via nonce/hash rewriting, applied to Tauri's own asset protocol. Build-time only — "no runtime per-request or per-origin customization." | Wrong shape twice over: (a) build-time static, can't express per-micro-app dynamic host grants; (b) only applies to the packaged asset protocol Hearth doesn't use for this content. |
| Generic real-`http(s)://` response interception (any Tauri/wry/WebKitGTK hook) | **Not found.** Open, unanswered upstream issue [tauri-apps/wry#1087](https://github.com/tauri-apps/wry/issues/1087) — a developer asking for exactly this ("equivalent to Tauri's `on_web_resource_request`... for real HTTP requests from the webview") with **no maintainer response and no known workaround** as of this research. WebKitGTK's public embedding API (`WebKitWebView`/`WebKitPolicyDecision`) exposes `decide-policy` for *allow/block* decisions on a response (`WebKitResponsePolicyDecision`), but not header **mutation** — it's read-and-veto, not read-and-modify. | Confirmed gap, not just an unlisted feature — this is the Electron `session.webRequest.onHeadersReceived` equivalent, and it does not exist. |

### Recommended workable design

Since no webview-layer hook applies to real network traffic, **don't try to
intercept inside the webview stack — move the enforcement point in front of
it.** Hearth already needs *something* to own the loopback HTTP server(s) that
serve the shell and each micro-app (today that's Vite dev servers spawned
from `electron/main/micro-apps/server.ts`). Put a small Rust-owned reverse
proxy (e.g. `hyper`/`axum` + `tokio-tungstenite` for the WebSocket upgrade)
between the webview and each real dev server:

- The webview still navigates to a real `http://127.0.0.1:PORT` URL (same
  *shape* the HMR spike validated — real loopback HTTP, no scheme change),
  just PORT now points at Hearth's proxy instead of the raw Vite process.
- The proxy forwards HTTP requests/responses transparently, and on the way
  out, strips any upstream `Content-Security-Policy` header and stamps its
  own — computed the same way `buildShellCsp()`/`buildMicroAppCsp()` do
  today, in Rust, from the same `CapabilityStore`-equivalent state.
- Because the proxy is Hearth's own trusted Rust code (not the
  agent-authored micro-app's Vite config), the "untrusted app can't strip
  it" property from the original comment in `session-policy.ts` is
  preserved — arguably *more* robustly, since it's now enforced outside the
  process the agent's code ever runs inside, rather than via a session hook
  that happens to sit above it.
- The Vite HMR WebSocket must be proxied too (`Upgrade: websocket`) — this
  is well-trodden for HTTP reverse proxies in Rust, but **is a new variable
  this research did not verify empirically**. It's a much narrower unknown
  than a scheme change, though: it doesn't touch the origin, the webview
  API, or WebKitGTK at all, only inserts one extra TCP hop that both HTTP
  and WS traffic must survive.

**Design considered and rejected:** proxying via
`register_uri_scheme_protocol` (i.e., make the whole `app://` custom scheme
the transport, reverse-proxying to Vite from inside the scheme handler).
Rejected because custom URI-scheme handlers in WebKitGTK
(`WebKitURISchemeRequest`/`Response`) are a request/response pattern with no
WebSocket-upgrade support — this would silently break Vite's HMR
`WebSocket`, directly reopening the risk §12 just closed. A same-origin,
same-scheme, same-protocol-family reverse proxy avoids that failure mode
entirely.

---

## 2. Permission gating (camera/mic/geolocation/etc.)

### What Hearth needs

`installSessionPolicy()` calls `session.setPermissionRequestHandler((_wc,
_permission, callback) => callback(false))` and
`session.setPermissionCheckHandler(() => false)` — an unconditional deny,
session-wide, no exceptions, for every powerful-feature permission Chromium
can ask about (camera, mic, geolocation, notifications, MIDI, etc.).

### Why Tauri's capability/ACL system is not the answer

Tauri v2's permission model ([v2.tauri.app/security/permissions](https://v2.tauri.app/security/permissions/))
is described in its own docs as controlling which **IPC commands** frontend
JS may invoke ("permissions... map scopes to commands and defines which
commands are enabled"); capabilities bind permissions to a window/webview.
This is Tauri's analog to Electron's `contextBridge`/preload allowlisting,
not to `session.setPermissionRequestHandler`. It has no concept of
browser-native `getUserMedia()`/`navigator.geolocation` prompts — those are
handled entirely inside the embedded browser engine (WebKitGTK on Linux),
below Tauri's IPC layer, and Tauri's own docs and Builder API (checked at
[docs.rs/tauri/2.11.5/tauri/webview/struct.Webview.html](https://docs.rs/tauri/2.11.5/tauri/webview/struct.Webview.html))
expose no method for it.

### What actually exists: WebKitGTK's native `permission-request` signal

WebKitGTK's public embedding API has always had this concept —
[`WebKitWebView::permission-request`](https://webkitgtk.org/reference/webkit2gtk/2.41.4/signal.WebView.permission-request.html):
fired for `WebKitGeolocationPermissionRequest`, `WebKitUserMediaPermissionRequest`
(camera/mic), `WebKitNotificationPermissionRequest`, and others; per WebKitGTK's
own docs, an unhandled request is **denied by default** (`webkit_permission_request_deny()`
is the fallback action), so Hearth's "deny by default" posture is even the
*native* default behavior, not something that has to be fought for.

Confirmed by reading the actual pinned crate source
(`webkit2gtk-rs`, tag `webkit2gtk-rs-v2.0.2`, matching this repo's
`Cargo.lock`):

- `src/auto/web_view.rs` (fetched via `gh api repos/tauri-apps/webkit2gtk-rs/contents/...`)
  defines, on the `WebViewExt` trait:
  ```rust
  #[doc(alias = "permission-request")]
  fn connect_permission_request<F: Fn(&Self, &PermissionRequest) -> bool + 'static>(
      &self,
      f: F,
  ) -> SignalHandlerId
  ```
  Per WebKitGTK's docs, returning `true` from this callback blocks the
  default handler (i.e. you've taken responsibility for the decision).
- `src/auto/permission_request.rs` defines `PermissionRequestExt`:
  ```rust
  fn allow(&self) { /* webkit_permission_request_allow */ }
  fn deny(&self)  { /* webkit_permission_request_deny */ }
  ```
  `PermissionRequest` is a GObject interface implemented by every concrete
  request type present in this crate version's `src/auto/`:
  `geolocation_permission_request.rs`, `user_media_permission_request.rs`
  (camera/mic), `notification_permission_request.rs`,
  `device_info_permission_request.rs`, `media_key_system_permission_request.rs`,
  `pointer_lock_permission_request.rs`,
  `install_missing_media_plugins_permission_request.rs`,
  `website_data_access_permission_request.rs` — i.e. every permission
  surface WebKitGTK exposes is covered by the same `deny()` call, no
  per-type branching required for a blanket "deny everything" policy.

### Reaching it from Tauri: `with_webview`

Tauri's `Webview`/`WebviewWindow` expose an escape hatch, confirmed at
[docs.rs/tauri/2.11.5/tauri/webview/struct.Webview.html](https://docs.rs/tauri/2.11.5/tauri/webview/struct.Webview.html):

```rust
pub fn with_webview<F: FnOnce(PlatformWebview) + Send + 'static>(
    &self,
    f: F,
) -> Result<()>
```

On Linux, `PlatformWebview` wraps the raw `webkit2gtk::WebView` (the doc
links straight to `docs.rs/webkit2gtk/2.0.0/webkit2gtk/struct.WebView.html`
and its `WebViewExt` trait — the same trait `connect_permission_request`
lives on). So the deny-by-default policy is expressible as:

```rust
webview.with_webview(|platform_webview| {
    let wv = platform_webview.inner(); // webkit2gtk::WebView
    wv.connect_permission_request(|_wv, request| {
        request.deny();
        true // handled — block WebKitGTK's own default action too
    });
})?;
```

called once per `WebviewWindow` at creation time.

### Whether this actually covers micro-apps

Checked `src/shell/MicroAppFrame.tsx` and `src/routes/micro.$name.tsx`:
micro-apps render as `<iframe>` elements **inside the shell's single
webview**, not as separate Tauri windows/webviews. `permission-request` is a
signal on the top-level `WebKitWebView`, fired regardless of which frame
(top document or a same-process iframe) issued the underlying
`getUserMedia()`/geolocation call — so **one `connect_permission_request`
call on the shell's single `WebviewWindow` covers the shell and every
embedded micro-app iframe**, matching Electron's session-wide reach with
even less wiring (no separate per-micro-app-window registration needed,
because there are no separate micro-app windows).

### Verdict for #2

"Deny by default, no exceptions" is fully expressible, per-window, via a
native WebKitGTK signal reached through Tauri's documented `with_webview`
escape hatch — not through Tauri's ACL/capability system, which is a
different (command-invocation) concern entirely. This is arguably a cleaner
fit than Electron's callback API: there's no `permission` string to
switch/allowlist against, just an unconditional `.deny()`.

---

## Overall assessment

Neither concern is a hard blocker for a Tauri port. Both require the same
shape of resolution the feasibility doc's own framing anticipated in §8d
("would likely require a custom Rust `WebviewWindow` request handler or
protocol-level intercept... a real (if bounded) design problem"):

- **CSP injection**: solved by moving the enforcement point to a Rust-owned
  reverse proxy in front of the dev servers, not by finding a matching
  webview API (none exists for real network traffic — confirmed against
  Tauri 2.11.5 / wry 0.55.1 docs and an open, unanswered upstream issue).
  New unverified variable: WebSocket-upgrade proxying for Vite HMR.
- **Permission gating**: solved by reaching WebKitGTK's native
  `permission-request` signal via Tauri's `with_webview` escape hatch and
  the pinned `webkit2gtk` 2.0.2 crate's `connect_permission_request`/`deny()`
  — confirmed present in the actual tagged source, not just general Tauri
  knowledge. No new unverified variable of consequence; this is desk-solid.

Both designs are source-verified but **not yet run**. Recommended next
spikes, in order of narrowness (cheapest/most isolated first):

1. **Permission-request denial**: extend the existing
   `spike/tauri-hmr-check/` shell with a `with_webview` +
   `connect_permission_request` call, load a page that calls
   `navigator.mediaDevices.getUserMedia()`, and confirm it's denied without
   a native OS prompt appearing. Half a day; no interaction with the HMR
   trick at all, essentially risk-free to verify.
2. **Reverse-proxy CSP injection**: stand up a minimal Rust HTTP+WS reverse
   proxy in front of the existing spike's Vite server, point the Tauri
   window at the proxy's port instead of Vite's own port, and re-run
   `spike/run-sequence.mjs`'s HMR assertions through the proxy hop to
   confirm the WebSocket upgrade survives and HMR still behaves identically
   to the §12 result. This is the one item here worth being cautious
   about — it's the only design in this document that touches the same
   code path §12 just de-risked.
