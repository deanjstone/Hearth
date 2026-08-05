// The agent's view_app/eval_js bridge (src-tauri/src/bridge.rs, Hearth#27
// Phase 2), against the real running app + real WebKitGTK — the third and
// last WebKitGTK-coupled behavior the testing strategy (#25/#37) names,
// alongside the HMR self-mod swap (hmr-self-mod.spec.js) and undo/redo
// (undo-redo.spec.js). Talks to the bridge exactly the way
// scripts/view-app.mjs and hearth-mcp-server.mjs do: read the per-boot
// URL/token the app wrote to .hearth/, then plain HTTP.
//
// Doesn't assert on route-capture *content*, or that it's pixel-identical
// to the main window before/after — the app's default route currently
// throws (window.hearth.workspaces.list() isn't ported yet, Hearth#41,
// tracked separately, out of scope here), and __root.tsx's onboarding
// self-heal effect + zustand's `persist` middleware mean a *second* window
// mounting the same app can legitimately change shared localStorage-backed
// UI state that the main window then re-renders with — a real
// characteristic of this app today, confirmed empirically (two back-to-back
// main-window captures differed after a route capture, even once settled),
// not a bridge bug. What's under test is the bridge mechanism itself: auth,
// eval-with-return (including error propagation), and that a route capture
// uses its own off-screen surface (not the main window) and leaves the main
// window still alive and capturable afterward — the same seam
// agent-bridge.ts's `createSnapshotWindow` provided on Electron.

import { existsSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { expect } from '@wdio/globals'

// Where the running app's own std::env::current_dir() landed when it wrote
// .hearth/bridge-url is a function of whatever launched it (tauri-driver,
// in turn launched by @wdio/tauri-service) — not something this spec
// controls. Try the repo root first (the working directory a developer or
// `cargo tauri dev` normally launches from), then this package's own cwd, as
// a fallback for whatever tauri-driver ends up using.
const REPO_ROOT = resolve(import.meta.dirname, '../..')
const CANDIDATE_DIRS = [REPO_ROOT, process.cwd()]

function findBridgeDir() {
  const dir = CANDIDATE_DIRS.find((d) => existsSync(resolve(d, '.hearth', 'bridge-url')))
  if (!dir) {
    throw new Error(
      `.hearth/bridge-url not found in any of: ${CANDIDATE_DIRS.join(', ')} — is the app running, and did it write the bridge files somewhere else?`,
    )
  }
  return resolve(dir, '.hearth')
}

function readBridge() {
  const dir = findBridgeDir()
  const base = readFileSync(resolve(dir, 'bridge-url'), 'utf8').trim()
  const token = readFileSync(resolve(dir, 'bridge-token'), 'utf8').trim()
  return { base, token }
}

async function bridgeEval(base, token, code) {
  const res = await fetch(`${base}/eval`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-hearth-token': token },
    body: JSON.stringify({ code }),
  })
  expect(res.status).toBe(200)
  return res.json()
}

async function bridgeSnapshot(base, token, path) {
  const url = path ? `${base}/snapshot?path=${encodeURIComponent(path)}` : `${base}/snapshot`
  const res = await fetch(url, { headers: { 'x-hearth-token': token } })
  expect(res.status).toBe(200)
  expect(res.headers.get('content-type')).toBe('image/png')
  return Buffer.from(await res.arrayBuffer())
}

const PNG_MAGIC = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])

describe('view_app/eval_js bridge', () => {
  let base
  let token

  before(async () => {
    ;({ base, token } = readBridge())

    // This spec runs first in the shared spec order and hits a freshly
    // booted app (hmr-self-mod.spec.js / undo-redo.spec.js implicitly get a
    // warm-up via their own $(selector) polling before asserting anything).
    // window.hearth is set by a synchronous side-effect import in
    // main.tsx, before React even renders — but Vite's dev-mode module
    // graph fetch can still take a beat on a cold session, so wait for it
    // rather than assume it's already there the instant the bridge answers.
    await browser.waitUntil(
      async () => {
        const result = await bridgeEval(base, token, '!!(window.hearth && window.hearth.selfMod && window.hearth.view)')
        return result.ok && result.result === true
      },
      { timeout: 15000, interval: 250, timeoutMsg: 'window.hearth.{selfMod,view} never became available' },
    )
  })

  it('rejects a request with the wrong bearer token', async () => {
    const res = await fetch(`${base}/eval`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-hearth-token': 'not-the-real-token' },
      body: JSON.stringify({ code: '1' }),
    })
    expect(res.status).toBe(401)
  })

  it('evaluates JS in the live renderer and returns a JSON result', async () => {
    const result = await bridgeEval(base, token, '1 + 1')
    expect(result).toEqual({ ok: true, result: 2 })
  })

  it('has window.hearth available in the live renderer (Phase 1 + Phase 2 IPC wired)', async () => {
    const result = await bridgeEval(
      base,
      token,
      '({ hasSelfMod: !!(window.hearth && window.hearth.selfMod), hasView: !!(window.hearth && window.hearth.view) })',
    )
    expect(result.ok).toBe(true)
    expect(result.result).toEqual({ hasSelfMod: true, hasView: true })
  })

  it('surfaces a thrown JS exception as {ok:false, error}, not a swallowed empty result', async () => {
    const result = await bridgeEval(base, token, '(function(){ throw new Error("bridge e2e probe") })()')
    expect(result.ok).toBe(false)
    expect(result.error).toBe('bridge e2e probe')
  })

  it('captures the live window as a real PNG', async () => {
    const png = await bridgeSnapshot(base, token, undefined)
    expect(png.subarray(0, 8)).toEqual(PNG_MAGIC)
    expect(png.length).toBeGreaterThan(100)
  })

  it('captures a route in an off-screen window and leaves the main window still capturable', async () => {
    const before = await bridgeSnapshot(base, token, undefined)
    expect(before.length).toBeGreaterThan(100)

    // Repeat route captures reuse the same cached off-screen window
    // (ensure_offscreen) — exercise that path too, not just first-creation.
    for (const path of ['/history', '/settings', '/history']) {
      const routePng = await bridgeSnapshot(base, token, path)
      expect(routePng.subarray(0, 8)).toEqual(PNG_MAGIC)
    }

    // The main window must still be the "main" webview specifically — no
    // path always targets it by construction (bridge.rs's capture_snapshot
    // hardcodes get_webview_window("main") for the no-path case) — and it
    // must still be alive and paintable after the off-screen window did its
    // own separate app boot + navigation. Not asserting byte-identity to
    // `before`: see this file's header comment for why that isn't a
    // guarantee this app currently makes.
    const after = await bridgeSnapshot(base, token, undefined)
    expect(after.subarray(0, 8)).toEqual(PNG_MAGIC)
    expect(after.length).toBeGreaterThan(100)
  })
})
