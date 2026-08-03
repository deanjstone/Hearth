# Research: WebKitGTK/Tauri UI capture + JS-eval-with-return capability survey

**Ticket:** [#16](https://github.com/deanjstone/Hearth/issues/16)
**Related:** `docs/decisions/rust-tauri-feasibility.md` §8c, §10 item 3, §12 (the
sibling Vite-HMR spike, already confirmed viable — not re-investigated here).

**Scope:** what does Tauri v2 + WebKitGTK actually offer, today, for the two
capabilities `electron/main/agent-bridge.ts` depends on:

1. `webContents.capturePage()` — pixel-perfect capture, including off-screen
   hidden windows.
2. `webContents.executeJavaScript(code, true)` — arbitrary JS with a returned,
   JSON-serialized result.

**Version pin used for every claim below:** this repo's own spike
(`spike/tauri-hmr-check/src-tauri/Cargo.lock`) pins `tauri = 2.11.5`,
`wry = 0.55.1`, `webkit2gtk = 2.0.2` (the Rust binding crate). `tauri 2.11.5`
was, as of this research (2026-08-04), also the latest published version on
crates.io (released 2026-07-01) — so this is current, not stale, information.

---

## 1. Capture

### 1a. Tauri v2 core / official plugins: no first-class capture API

Tauri's `tauri` crate (checked against the pinned `2.11.5` docs.rs page for
`tauri::webview::WebviewWindow`) exposes no `capture`, `screenshot`, or
`snapshot` method. The only rendering-adjacent method is `print()` (opens a
native print dialog; "Currently only supported on macOS on `wry`" per its own
doc text — not usable as a programmatic pixel-capture path). Tauri's official
plugin workspace (`tauri-plugin-*` under github.com/tauri-apps) has no
first-party screenshot/snapshot plugin as of this check.

Community plugins exist but are not a drop-in answer:
- [`tauri-plugin-screenshots`](https://github.com/ayangweb/tauri-plugin-screenshots)
  — OS-level window/monitor screenshots (i.e. it screenshots whatever is
  actually on screen), not a webview-content capture API; not useful for
  capturing a route in a hidden window.
- [`tauri-plugin-snapshot`](https://github.com/Fractal-Tess/tauri-plugin-snapshot)
  (Fractal-Tess) — webview-content capture (`snapshotViewport()` /
  `snapshotDocument()`), and its `Cargo.toml` pins `tauri = "1.0.0"` and
  `webkit2gtk = "0.18.2"` — this is a **Tauri v1** plugin, unverified against
  this repo's v2/wry-0.55.1 stack, and its own README states
  `"DISCLAIMER: This plugin is missing a MacOS implementation!!!"`. Not
  production-usable as-is, but its source is a useful worked example (see
  1b) because it demonstrates the exact native call chain that also exists
  in the v2/wry-0.55.1 stack this repo uses.

Upstream trackers confirm this is a known, still-open gap, not something
actively being closed:
- [`tauri-apps/wry#1358`](https://github.com/tauri-apps/wry/issues/1358)
  ("Add screenshot capability") — open, requesting an
  `async capture_screenshot() -> Bytes` method on `WebView` for testing/error
  reporting. No maintainer commitment visible.
- [`tauri-apps/wry#70`](https://github.com/tauri-apps/wry/issues/70) —
  closed (2021), same request, no implementation landed from it.
- [`tauri-apps/tao#289`](https://github.com/tauri-apps/tao/issues/289) —
  open since January 2022, requesting off-screen rendering support in `tao`
  (the windowing crate under wry/tauri); still open, no maintainer
  resolution, 4+ years on. This is the clearest signal that "capture an
  off-screen/hidden window" specifically is an unresolved upstream gap, not
  just an undocumented-but-easy operation.

**Conclusion: no wrapped, ready-to-use "capture this WebviewWindow to a PNG"
API exists in Tauri core, an official plugin, or a maintained community
plugin for this stack.**

### 1b. WebKitGTK's own native snapshot API — does exist, and is reachable from Rust

WebKitGTK itself has always had a native snapshot primitive, independent of
Tauri:

```c
void webkit_web_view_get_snapshot (WebKitWebView* web_view,
                                    WebKitSnapshotRegion region,
                                    WebKitSnapshotOptions options,
                                    GCancellable* cancellable,
                                    GAsyncReadyCallback callback,
                                    gpointer user_data);
cairo_surface_t* webkit_web_view_get_snapshot_finish (WebKitWebView *web_view,
                                                        GAsyncResult *result,
                                                        GError **error);
```
(webkitgtk.org WebKit2.WebView reference — signature stable across the
2.x/2.4x doc pages checked.) It's async (GLib pattern), takes a region
(`WEBKIT_SNAPSHOT_REGION_VISIBLE` or `_FULL_DOCUMENT`), and hands back a
Cairo surface.

**This is reachable from this repo's exact pinned Rust stack**, via a
concrete chain verified across three primary sources at the pinned versions:

1. `tauri::webview::WebviewWindow::with_webview(closure)` (docs.rs,
   `tauri` `2.11.5`) — runs a closure on the main thread with a
   `PlatformWebview` handle to the native webview.
2. `tauri::webview::PlatformWebview::inner()` (docs.rs, `tauri` `2.11.5`,
   gated on `target_os` = linux/dragonfly/freebsd/netbsd/openbsd) — doc text:
   *"Returns `webkit2gtk::WebView` handle."* This is Tauri's own documented
   escape hatch to the raw WebKitGTK object, no FFI required.
3. `webkit2gtk::WebViewExt::snapshot()` / `snapshot_future()` (docs.rs,
   `webkit2gtk` `2.0.1` — the last version docs.rs built successfully
   adjacent to this repo's pinned `2.0.2`, which failed to build docs but is
   a patch-level bump of the same crate) — a safe Rust binding directly over
   `webkit_web_view_get_snapshot`/`_finish`, with signature:
   ```rust
   fn snapshot(&self, region: SnapshotRegion, options: SnapshotOptions,
               cancellable: Option<&impl IsA<Cancellable>>, callback: P);
   // + snapshot_future() -> Pin<Box<dyn Future<Output = Result<Surface, Error>>>>
   ```

This exact chain is not hypothetical — it's what `tauri-plugin-snapshot`'s
`src/plugin/linux.rs` actually does (confirmed by reading its source):
`webview.inner().snapshot(region, snapshot_options, Cancellable::NONE, move
|surface| { ... })`, converting the resulting surface via
`cairo::ImageSurface::try_from(surface)` and `write_to_png()` into a PNG
buffer sent back through a channel. That plugin targets Tauri v1's older
API shape, but the underlying `webkit2gtk::WebView::snapshot` call is the
same primitive available in this repo's v2/wry-0.55.1/webkit2gtk-2.0.2
stack — Tauri v2 just reaches it via `with_webview()`/`PlatformWebview`
instead of v1's equivalent.

Separately, `wry::WebViewExtUnix::webview(&self) -> webkit2gtk::WebView`
(docs.rs, `wry` `0.55.1`, "Returns Webkit2gtk Webview handle") is a second,
independent confirmation of the same escape hatch one layer down (Tauri's
`PlatformWebview::inner()` is effectively a thin wrapper over this).
`WebViewExtUnix` also exposes `reparent(&self, widget: &W)` — *"Attaches the
webview to a given widget and removes it from its current one"* — which
matters for the off-screen question below.

**Conclusion: WebKitGTK's native snapshot API is real, current, and directly
callable from this repo's pinned Rust dependency versions with no FFI and no
unofficial patches — just `with_webview()` → `.inner()` → `.snapshot()`.
This would need to be hand-rolled (a small amount of Rust, following the
`tauri-plugin-snapshot` linux.rs source as a template), since no maintained
plugin ships it for this stack.**

### 1c. Off-screen / hidden window capture — the genuinely open question

This is the part Electron's `capturePage()` on a hidden `BrowserWindow`
makes trivial and where WebKitGTK does **not** have an equally clean answer,
and where I could not find a source that settles it definitively for the
Tauri-specific case:

- GTK has a long-established pattern for capturing widgets that are *never
  shown on screen at all*: `GtkOffscreenWindow`
  ([GTK3 docs](https://docs.gtk.org/gtk3/class.OffscreenWindow.html)) — "a
  toplevel container widget ... used to retrieve snapshots of widgets
  without showing them on the screen." There's a documented, working
  pattern of embedding a `WebKitWebView` inside a `GtkOffscreenWindow` and
  calling `webkit_web_view_get_snapshot()` on it (mailing-list threads and a
  standalone example gist confirm this is a real, if old, technique). A
  historical WebKitGTK mailing-list report flagged a segfault regression
  rendering to `GtkOffscreenWindow` between WebKitGTK 2.46.1 and 2.48.1 —
  I could not confirm current status of that regression against the WebKitGTK
  version this repo would actually ship against, so treat GtkOffscreenWindow
  as "known technique, not verified stable on a current WebKitGTK."
- Tauri's `WebviewWindowBuilder::visible(bool)` (docs.rs, `tauri` `2.11.5`,
  doc text: *"Whether the window should be immediately visible upon
  creation"*) lets you create a window that starts hidden — this is the
  direct analogue of Electron's hidden `BrowserWindow`. **What's unverified:
  whether an invisible/never-shown `gtk::ApplicationWindow` (Tauri's default
  toplevel container for a `WebviewWindow` on Linux) still realizes its
  child `webkit2gtk::WebView` enough for `snapshot()` to produce real
  content**, versus needing to actively reparent the webview into a
  `GtkOffscreenWindow` via wry's `WebViewExtUnix::reparent()` to guarantee a
  render happens. I found no doc, changelog, or issue thread that answers
  this specifically for Tauri's window setup — it's an empirical question
  that needs a spike, not a documentation read.
- **X11 vs Wayland is a real fork, but only for the OS-level fallback path,
  not for the WebKit-native snapshot path.** `webkit_web_view_get_snapshot()`
  reads WebKit's own internal Cairo-composited render surface — it does not
  ask the window-system compositor for pixels, so it should be display-server
  agnostic (same call, same behavior under Xorg or Wayland) — though I did
  not find a source stating this explicitly either way; it's an inference
  from what the API captures (an application-level Cairo surface, not a
  compositor screenshot), flagged here as [speculation, moderate confidence].
  The *OS-level compositor screenshot fallback* the feasibility doc mentions
  is genuinely split by session type: on X11, `gdk_pixbuf_get_from_window()`
  / `XGetImage` can read pixels from essentially any window including
  obscured/off-screen ones directly, no permission prompt. On Wayland, there
  is no such direct access by design — the only sanctioned path is
  `xdg-desktop-portal`'s `org.freedesktop.portal.Screenshot`, which (per
  [flatpak/xdg-desktop-portal#1093](https://github.com/flatpak/xdg-desktop-portal/issues/1093))
  requires a per-request interactive permission dialog it can't even
  associate with a surface for apps with no visible window at request time —
  i.e. it is not usable for headless/hidden-window capture at all on
  Wayland. **This makes the WebKit-native `snapshot()` path (1b) the only
  viable capture route if hidden-window capture on Wayland needs to work**;
  the OS-level fallback only works on X11, and even there it doesn't cleanly
  support "capture a window nobody ever mapped."

---

## 2. Eval with return value

### 2a. What existed until recently: fire-and-forget `eval()`

`tauri::webview::WebviewWindow::eval(&self, js) -> Result<()>` (docs.rs,
`tauri` `2.11.5`) — *"Evaluates JavaScript on this window."* Returns
`Result<()>`, i.e. success/failure of dispatching the script, never the
script's own result. This is the same shape as
`wry::WebView::evaluate_script(&self, js: &str) -> Result<()>` (docs.rs,
`wry` `0.55.1`) that it wraps. Until recently, the only way to get a value
back was exactly what the feasibility doc guessed: have the injected JS
`invoke()` back into Rust (or emit an event) with the result, manually
wiring up a request/response correlation — this is what
[`tauri-apps/tauri#5441`](https://github.com/tauri-apps/tauri/issues/5441)
("expose wry's `eval_with_callback` in tauri", opened 2022-10-19) was tracking.

### 2b. What's actually shipped now: `eval_with_callback` — new as of Tauri 2.11.0

**This is the one place the feasibility doc's guess is now outdated by an
upstream change, and it's recent enough that most existing blog posts/Stack
Overflow answers on this topic predate it.** Issue #5441 above was closed by
[`tauri-apps/tauri#14925`](https://github.com/tauri-apps/tauri/pull/14925)
("feat: add `eval_with_callback` to Webview and WebviewWindow"), merged
2026-04-01, and — per the PR's own package version bump — **shipped in Tauri
2.11.0**. This repo's pinned `2.11.5` is later than that, so it has it.

Confirmed directly on the pinned-version docs.rs page for
`tauri::webview::WebviewWindow` (`2.11.5`):

```rust
pub fn eval_with_callback(
    &self,
    js: impl Into<String>,
    callback: impl Fn(String) + Send + 'static,
) -> Result<()>
```
Doc text: *"Evaluate JavaScript with callback function on this webview. The
evaluation result will be serialized into a JSON string and passed to the
callback function."*

This wraps `wry::WebView::evaluate_script_with_callback` (docs.rs, `wry`
`0.55.1`, identical doc text/shape) directly — i.e. Tauri now exposes wry's
callback-based eval one-to-one, no manual `invoke`/event plumbing on the JS
side required.

**Caveat — this is still not a `Future`/awaitable return, just a more
direct callback.** `eval_with_callback` itself still returns
`Result<()>` (dispatch success/failure) and delivers the actual result later
via the `Fn(String)` callback on the main thread — it does not hand back
`impl Future<Output = String>`. To get an `async`/`await`-shaped call out of
it (which `agent-bridge.ts`'s `eval_js` semantics would want — a promise
that resolves with the JSON result), the standard, well-established pattern
(seen across multiple current sources, e.g. the `tauri-plugin-pilot`-style
IPC-callback pattern and general Tauri async-command guidance) is a
`tokio::sync::oneshot` channel: create `(tx, rx)`, move `tx` into the
`eval_with_callback` closure, `tx.send(result)` when it fires, `await rx` (with
a timeout) at the call site. This is a small, well-understood adapter — a few
lines of glue — not a redesign.

**Conclusion: getting a return value out of Tauri v2's eval no longer
requires the invoke/event round-trip the feasibility doc guessed at. As of
Tauri 2.11.0 (already in this repo's pinned 2.11.5), `eval_with_callback` is
a direct, one-hop path from Rust to a JSON-serialized JS result, needing only
a thin `oneshot`-channel wrapper to present it as an awaitable Rust
`Future`.** This significantly de-risks §10 item 3's eval half specifically.

---

## 3. Overall verdict for a follow-on prototype spike

| Capability | Status | Confidence |
|---|---|---|
| Eval-with-return | **Resolved.** `eval_with_callback` (Tauri ≥2.11.0, confirmed present in pinned 2.11.5) + a `tokio::oneshot` wrapper gives an awaitable JS-eval-with-JSON-result, directly replacing `executeJavaScript(code, true)`. | High — read directly off the pinned-version docs.rs pages and the merging PR. |
| Capture, visible window | **Buildable, not wrapped.** `with_webview()` → `PlatformWebview::inner()` → `webkit2gtk::WebViewExt::snapshot()` reaches WebKitGTK's real native snapshot API using only this repo's pinned crate versions, following the pattern already proven (for an older Tauri version) by `tauri-plugin-snapshot`'s Linux backend. No existing plugin ships this cleanly for Tauri v2/wry 0.55 — it'd be ~50-150 lines of hand-rolled Rust. | Medium-high — the API chain is confirmed at every link, but no maintained crate exercises the exact v2 combination end-to-end. |
| Capture, hidden/off-screen window | **Open — needs a spike, not more reading.** Whether an invisible Tauri `WebviewWindow` (or one reparented into a `GtkOffscreenWindow` via `WebViewExtUnix::reparent()`) still produces a real `snapshot()` result is not documented anywhere I found. `tao#289` (open since 2022) suggests this exact gap (off-screen rendering in the tao/wry windowing layer) is a known, unresolved upstream limitation, not just an obscure detail. On Wayland specifically, there is no usable OS-level fallback for this (portal screenshot requires an interactive per-request permission dialog and doesn't support windowless capture) — so if the native `snapshot()` path doesn't work on a hidden window, there may be **no** viable fallback for `view_app({ path: ... })`'s "capture a route in a hidden window without disturbing the user's view" use case on Wayland. | Low — genuinely unresolved by documentation; this is the actual remaining risk. |

**Recommendation:** the eval-with-return half of §10 item 3 can be
considered de-risked by this research alone — no further spike needed there,
just implement `eval_with_callback` + `oneshot` when the port happens. The
capture half is where a narrowly-scoped follow-on spike is still warranted,
and it should be scoped specifically to the open question in the table
above: build the minimal `with_webview()` → `.inner()` → `.snapshot()` chain
against an intentionally **hidden** (`visible(false)`) `WebviewWindow` on
this machine's actual WebKitGTK/GTK version, and separately try the
`GtkOffscreenWindow` + `reparent()` variant if the direct approach doesn't
render real content. That's a single, cheap, answerable yes/no test — not a
redesign — and it would fully close out §10 item 3.

---

## Sources consulted (primary, version-pinned where applicable)

- `tauri` crate docs.rs, pinned version `2.11.5`: `webview::WebviewWindow`,
  `webview::PlatformWebview`, `webview::WebviewWindowBuilder`.
- `wry` crate docs.rs, pinned version `0.55.1`: `WebView`, `WebViewExtUnix`.
- `webkit2gtk` crate docs.rs, version `2.0.1` (latest successfully-built
  docs adjacent to this repo's pinned `2.0.2`): `WebViewExt` (`snapshot`,
  `snapshot_future`).
- webkitgtk.org WebKit2.WebView C API reference (`get_snapshot`,
  `get_snapshot_finish`).
- docs.gtk.org GTK3 `Gtk.OffscreenWindow` reference.
- `github.com/tauri-apps/tauri` — issue #5441, PR #14925 (merged
  2026-04-01, shipped in tauri 2.11.0).
- `github.com/tauri-apps/wry` — issues #70 (closed), #1358 (open).
- `github.com/tauri-apps/tao` — issue #289 (open since 2022-01-19).
- `github.com/Fractal-Tess/tauri-plugin-snapshot` — README, `Cargo.toml`,
  `src/plugin.rs`, `src/plugin/linux.rs` (read directly; confirms the
  `.inner().snapshot(...)` call chain in working code, albeit on Tauri v1).
- `github.com/ayangweb/tauri-plugin-screenshots` — README (ruled out as
  OS-level-only, not webview-content capture).
- `github.com/flatpak/xdg-desktop-portal` — issue #1093 (Wayland Screenshot
  portal permission/no-surface limitation).
- crates.io `tauri` version index (confirms `2.11.5` is current as of this
  research, released 2026-07-01).
- This repo: `spike/tauri-hmr-check/src-tauri/Cargo.toml` /
  `Cargo.lock` (source of the pinned version numbers used throughout), and
  `docs/decisions/rust-tauri-feasibility.md` §8c/§10/§12 for the original
  framing this research answers.
