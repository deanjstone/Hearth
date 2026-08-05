// The agent's view_app/eval_js bridge (src-tauri/src/bridge.rs, Hearth#27
// Phase 2), against the real running app + real WebKitGTK — the third and
// last WebKitGTK-coupled behavior the testing strategy (#25/#37) names,
// alongside the HMR self-mod swap (hmr-self-mod.spec.js) and undo/redo
// (undo-redo.spec.js). Talks to the bridge exactly the way
// scripts/view-app.mjs and hearth-mcp-server.mjs do: read the per-boot
// URL/token the app wrote to .hearth/, then plain HTTP.
//
// Doesn't assert on route-capture *content* — the app's default route
// currently throws (window.hearth.workspaces.list() isn't ported yet,
// Hearth#41, tracked separately, out of scope here) so a captured route's
// pixels aren't meaningful yet. What's under test is the bridge mechanism
// itself: auth, eval-with-return (including error propagation), and that a
// route capture creates its own off-screen surface without touching the
// user's actual window — the same guarantee agent-bridge.ts's
// `createSnapshotWindow` gave on Electron.

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

  it('captures a route in an off-screen window without disturbing the main window', async () => {
    // Establish a *stable* baseline first: right after boot the main window
    // can still be mid-paint (or mid-crash-recovery — see the file header's
    // note on #41) between two captures a few hundred ms apart, which isn't
    // what this test is about. Poll until two consecutive captures agree,
    // then that's the baseline route-capture must not disturb.
    let stableBaseline = null
    await browser.waitUntil(
      async () => {
        const a = await bridgeSnapshot(base, token, undefined)
        const b = await bridgeSnapshot(base, token, undefined)
        if (a.equals(b)) {
          stableBaseline = b
          return true
        }
        return false
      },
      { timeout: 15000, interval: 300, timeoutMsg: 'the main window never settled into two consecutive identical captures' },
    )

    const routePng = await bridgeSnapshot(base, token, '/history')
    expect(routePng.subarray(0, 8)).toEqual(PNG_MAGIC)

    // The user's actual window must come back byte-identical to the settled
    // baseline — nothing about capturing a different route should have
    // touched it.
    const after = await bridgeSnapshot(base, token, undefined)
    expect(after.equals(stableBaseline)).toBe(true)
  })
})
