# Working in Hearth

Hearth is a self-evolving desktop client for coding agents. When you run inside
it, **this repo is the running app** and you may be asked to change its own UI.

- The renderer (`src/**`) is served by a live Vite dev server, so your edits
  **hot-reload into the running app** — no restart needed.
- The live shell is **Rust/Tauri** (`src-tauri/**`), not Electron, as of the
  Phase 7 cutover (tracking issue #27). `electron/main/**` and
  `electron/preload/**` are retired from active use (kept in git history —
  see `docs/decisions/` for the cutover ADR) — there is no reason to edit
  them. Editing `electron/preload-tauri/**` or `src/lib/hearth-bridge-tauri.ts`
  is the equivalent of "editing preload" for this shell.
- Editing `src-tauri/**` needs a real recompile + process restart — it does
  **not** hot-reload, unlike `src/**`. `cargo tauri dev` (via `pnpm dev`)
  auto-restarts on Rust-side changes; this is slower than a renderer edit,
  so prefer changes in `src/**` when the task allows it.
- Your edits are auto-committed as `Hearth-SelfMod` git commits and are revertable
  from the app's History view, so make focused changes.

## Seeing and controlling the live app

Hearth gives you MCP tools (server `hearth`) to both **see** and **drive** the
running app — anything the user could do. The window itself isn't visible to you,
so use these.

See:
- `view_app()` — screenshot the current screen. `view_app({ path: "/history" })`
  renders+captures a specific route in a hidden window (doesn't disturb the user's
  view). Always look after a UI change before claiming it works.
- `read_ui()` — list interactive elements (buttons/links/inputs) with their text
  and a CSS selector. Use it to find what to act on.

Control:
- `click({ text: "Send" })` or `click({ selector: "..." })`
- `fill({ selector: "input", value: "...", submit: true })`
- `eval_js({ code: "..." })` — run arbitrary JS in the renderer. It has the DOM and
  `window.hearth` (every IPC: `agent.prompt`, `selfMod.undo`, `microApps.start`,
  `agent.setBackend`, …). Make the final expression return something serializable.

A good loop: `read_ui` (or `view_app`) to orient → `click`/`fill`/`eval_js` to act
→ `view_app` to confirm.

Shell fallback for screenshots: `node scripts/view-app.mjs [route]` saves a PNG to
`.hearth/snapshot.png`. (The raw dev URL `http://localhost:5173` will NOT render
outside the app — it needs the preload bridge.)
