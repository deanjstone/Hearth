# Spike: does Tauri/WebKitGTK survive Hearth's self-mod HMR trick?

See `docs/decisions/rust-tauri-feasibility.md` §10.1 (question) and §12
(results). Verdict: **confirmed viable** — 6/6 criteria passed identically
against both an Electron control and a Tauri/WebKitGTK test run.

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
