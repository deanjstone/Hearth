# Spike: does Tauri/WebKitGTK survive Hearth's self-mod HMR trick?

See `docs/decisions/rust-tauri-feasibility.md` §10.1 (question) and §12
(results). Verdict: **confirmed viable** — 6/6 criteria passed identically
against both an Electron control and a Tauri/WebKitGTK test run.

## Spike: does `webkit_web_view_get_snapshot()` capture a hidden/off-screen window?

See [wayfinder ticket #17](https://github.com/deanjstone/Hearth/issues/17)
and `docs/decisions/rust-tauri-feasibility.md` §10.3. Verdict: **`visible(false)`
does not work** — WebKitGTK never realizes/maps the widget, so the snapshot
call reliably fails with a generic glib error. **A visible-but-off-screen
window does work** — `.visible(true)` with `.position(-32000.0, -32000.0)`,
`.decorations(false)`, `.skip_taskbar(true)`, `.focused(false)` produces a
correct, real-pixel PNG every time, confirmed by visual inspection of the
output. This is the workaround Hearth's own hidden-capture path (`view_app`)
needs to use in the Rust port, not the naive `visible(false)` approach.

Reproduce (same WebKitGTK/Linux prerequisites as the HMR spike below):

```sh
(cd spike/tauri-hidden-capture/src-tauri && cargo run)
```

Writes `snapshot-hidden-false.png` (only if that scenario somehow succeeds —
expected to be absent) and `snapshot-offscreen.png` into
`spike/tauri-hidden-capture/`, and prints a `SCENARIO_RESULT[...]` line per
scenario to stdout.

## Spike: does a Rust reverse proxy in front of Vite inject CSP headers and pass through the HMR WebSocket upgrade?

See [wayfinder ticket #22](https://github.com/deanjstone/Hearth/issues/22)
and #20's locked CSP design this prototypes. Verdict: **yes to both.**
(a) On every normal HTTP response the proxy strips any upstream
`Content-Security-Policy` header and stamps its own fixed value —
confirmed via `curl` header comparison against the same request made
directly to Vite, plus a body-diff proving the swap doesn't corrupt
content. (b) Vite's HMR client targets wherever the page's own script was
loaded from (no `hmr.clientPort`/`hmr.host` override in
`vite.config.spike.ts`), so routing the page through the proxy makes the
HMR WebSocket transit the proxy too — confirmed by editing the marker
module and watching the new value land in the marker log over the same
already-spliced connection, corroborated by Vite's own `hmr update` log
line and the proxy's `SPLICE_START`/`SPLICE_END` evidence (no
`SPLICE_ERROR` during actual use — the one seen in testing came from
force-killing the app afterward, not from proxy misbehavior).

Reproduce:

```sh
# 1. Standalone Vite server (repo root, port 5183) — same one the HMR spike uses
pnpm exec vite --config spike/vite.config.spike.ts

# 2. The proxy (port 5199, forwards to 5183)
(cd spike/tauri-csp-proxy/proxy && cargo run)

# 3. The Tauri test app — same Linux/WebKitGTK prerequisites as the HMR spike.
#    Its tauri.conf.json points devUrl/windows[0].url at the proxy
#    (http://localhost:5199/spike/tauri-csp-proxy/proxy-check.html), not at
#    Vite directly, so this is the only route the window has to the dev server.
(cd spike/tauri-csp-proxy/tauri-app/src-tauri && cargo run)

# 4. Verify (a): compare headers between direct-to-Vite and through-the-proxy
curl -sD - -o /dev/null http://localhost:5183/spike/tauri-csp-proxy/proxy-check.html
curl -sD - -o /dev/null http://localhost:5199/spike/tauri-csp-proxy/proxy-check.html

# 5. Verify (b): with the app running, edit src/spike-csp-proxy-marker.ts's
#    MARKER_VALUE, save, then check the marker log through the proxy:
curl -s http://localhost:5199/__spike/marker
```

Layout: `proxy/` is the Rust reverse proxy (hyper 1.x, one upstream, no TLS,
no pooling — the smallest thing that answers the question). `tauri-app/` is
a near-verbatim copy of `tauri-hmr-check/` pointed at the proxy instead of
Vite. `proxy-check.html` is a dedicated static entry page (not the real
Hearth app) that loads `src/spike-csp-proxy-marker.ts`.

## Layout

- `tauri-hmr-check/` — minimal Tauri v2 shell. No IPC layer, no ACP, no
  bundled frontend. `src-tauri/tauri.conf.json`'s `app.windows[0].url`
  points at `vite.config.spike.ts`'s dev server; that's the entire app.
- `vite.config.spike.ts` — standalone Vite dev server reusing the same
  renderer plugins as `electron.vite.config.ts` (`selfModOverlay`,
  `tanstackRouter`, `react`, `tailwind`), plus a dev-only
  `/__spike/marker` endpoint for observing HMR behavior over HTTP.
- `run-sequence.mjs` — drives a real self-mod turn through
  `/__hearth/self-mod` (pin → edit-on-disk → apply → turn-start/turn-end)
  and asserts against the marker log. Run with `--host=electron` or
  `--host=tauri`.
- `results-electron.json` / `results-tauri.json` — the recorded pass/fail
  output from the last run of each.

## Reproducing

```sh
# 1. Standalone Vite server (repo root, port 5183)
pnpm exec vite --config spike/vite.config.spike.ts

# 2a. Electron control — build once, then point ELECTRON_RENDERER_URL at
#     the spike server instead of electron-vite's own dev server.
pnpm exec electron-vite build
ELECTRON_RENDERER_URL=http://localhost:5183 pnpm exec electron .
node spike/run-sequence.mjs --host=electron

# 2b. Tauri test — requires the Rust toolchain (rustup) and, on Linux,
#     WebKitGTK dev headers (libwebkit2gtk-4.1-dev + the usual Tauri Linux
#     prerequisites).
(cd spike/tauri-hmr-check/src-tauri && cargo run)
node spike/run-sequence.mjs --host=tauri
```

`spike/*/target/` and `spike/*/node_modules/` are gitignored — only source
and results are committed.
