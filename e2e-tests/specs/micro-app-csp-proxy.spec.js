// The micro-app CSP reverse proxy + WebKitGTK permission-request deny-all
// (src-tauri/src/micro_apps/csp_proxy.rs, src-tauri/src/webview_hardening.rs,
// Hearth#27 Phase 6), against the real running app + real WebKitGTK — the
// third and last WebKitGTK-coupled behavior the testing strategy (#25/#37)
// names, alongside the HMR self-mod swap (hmr-self-mod.spec.js) and the
// capture/eval bridge (capture-eval-bridge.spec.js).
//
// src-tauri's own unit/integration tests (micro_apps/csp_proxy.rs) already
// cover the proxy's header-injection and per-request-CSP logic against a
// fake hyper upstream — real, but not the thing #27's Phase 6 bullet asks
// for ("Extend the WebDriver suite to cover the CSP proxy end-to-end").
// What's under test here is everything those unit tests can't reach: the
// real `window.hearth.microApps.start()` command actually spawning a real
// Vite dev server and a real proxy in front of it inside the real app, and
// the permission-request handler actually being wired into the real
// WebKitGTK webview at startup (lib.rs's `setup()` — pure glue code with no
// Rust-side test coverage of its own).
//
// The fixture app the running Tauri process resolves `micro-apps/<name>`
// against is at `micro-apps/e2e-csp-fixture/` — but which directory the
// process treats as its repo_root depends on tauri-driver's own launch cwd,
// which capture-eval-bridge.spec.js's own `findBridgeDir` comment already
// notes isn't something a spec controls (observed in CI: sometimes the git
// root, sometimes e2e-tests/ itself, the cwd `pnpm run test` runs from).
// `e2e-tests/micro-apps/e2e-csp-fixture` is a symlink back to the real one
// (not a copy — avoids drift) so the fixture resolves correctly either way.
//
// Talks to the bridge exactly like capture-eval-bridge.spec.js. eval_js's
// wrapper (bridge.rs's `wrap_eval_code`) substitutes the given code directly
// into `(code)` — a single EXPRESSION position, not a statement sequence, so
// it does NOT await a Promise and can't contain top-level `;`-separated
// statements (only the comma operator chains multiple side effects in an
// expression position). `window.hearth.microApps.start(...)` (async) and
// `navigator.geolocation.getCurrentPosition` (callback-based) both
// fire-and-stash a result on `window` via a comma-expression, then a
// separate `browser.waitUntil` polls for it — same fire-and-poll shape this
// file's sibling specs already use for async state.

import { existsSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { expect } from '@wdio/globals'

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

const FIXTURE_APP = 'e2e-csp-fixture'

describe('micro-app CSP proxy + permission deny-all', () => {
  let base
  let token
  let proxyUrl

  before(async () => {
    ;({ base, token } = readBridge())

    await browser.waitUntil(
      async () => {
        const result = await bridgeEval(base, token, '!!(window.hearth && window.hearth.microApps)')
        return result.ok && result.result === true
      },
      { timeout: 15000, interval: 250, timeoutMsg: 'window.hearth.microApps never became available' },
    )

    // Fire-and-stash: start() is async (spawns a real Vite process on
    // first call — pnpm-installs the fixture's one dependency if this is a
    // cold checkout, so this can take a while), so its resolution can't be
    // read back through the same synchronous eval call.
    await bridgeEval(
      base,
      token,
      `(window.__e2eMicroAppStart = null,
        window.hearth.microApps.start(${JSON.stringify(FIXTURE_APP)})
          .then((url) => { window.__e2eMicroAppStart = { ok: true, url } })
          .catch((e) => { window.__e2eMicroAppStart = { ok: false, error: String(e) } }),
        true)`,
    )

    await browser.waitUntil(
      async () => {
        const result = await bridgeEval(base, token, 'window.__e2eMicroAppStart')
        return result.ok && result.result !== null
      },
      // Generous: a cold checkout's first start pnpm-installs vite itself.
      { timeout: 45000, interval: 500, timeoutMsg: 'micro-app start() never settled' },
    )

    const started = (await bridgeEval(base, token, 'window.__e2eMicroAppStart')).result
    if (!started.ok) {
      throw new Error(`microApps.start(${FIXTURE_APP}) failed: ${started.error}`)
    }
    proxyUrl = started.url
  })

  after(async () => {
    await bridgeEval(base, token, `(window.hearth.microApps.stop(${JSON.stringify(FIXTURE_APP)}), true)`)
  })

  it('returns a proxy URL, not Vite\'s raw dev-server URL', async () => {
    // The real Vite dev server picks its own port (5173+ scanning); the
    // proxy binds a *different*, OS-assigned port in front of it. The only
    // externally-checkable evidence a proxy is actually in the path (vs.
    // the renderer just getting handed Vite's URL straight, Electron-style)
    // is that this URL round-trips through a listener that stamps CSP —
    // asserted below — but at minimum it must be a real http://127.0.0.1
    // loopback URL.
    expect(proxyUrl).toMatch(/^http:\/\/127\.0\.0\.1:\d+/)
  })

  it('stamps the authoritative CSP on the real proxied response', async () => {
    const res = await fetch(proxyUrl)
    expect(res.status).toBe(200)
    const csp = res.headers.get('content-security-policy')
    expect(csp).toBeTruthy()
    expect(csp).toContain("default-src 'self'")
    expect(csp).toContain("object-src 'none'")
    // connect-src must scope to exactly this proxy's own origin, its ws
    // upgrade, and the broker (if running) — an ungranted fixture app
    // reaches nothing else. The broker is started unconditionally at app
    // boot (lib.rs's setup()), so its origin is expected to be present; it
    // was handed back in the start() URL's own __hearthBroker query param
    // (micro_apps_commands.rs's micro_app_start), so read the expectation
    // from there rather than hardcoding "broker is always running".
    const proxyOrigin = new URL(proxyUrl).origin
    const wsOrigin = proxyOrigin.replace('http', 'ws')
    const brokerOrigin = new URL(proxyUrl).searchParams.get('__hearthBroker')
    const expectedHosts = ["'self'", wsOrigin, ...(brokerOrigin ? [brokerOrigin] : [])]
    const connect = csp.split('; ').find((d) => d.startsWith('connect-src '))
    expect(connect).toBe(`connect-src ${expectedHosts.join(' ')}`)
    // No external host is reachable — nothing was approved for this fixture.
    expect(connect).not.toContain('https://')
  })

  it('proxies real Vite content through, not just headers', async () => {
    const res = await fetch(proxyUrl)
    const body = await res.text()
    expect(body).toContain('e2e-csp-fixture-loaded')
  })

  it('every request through the proxy carries the CSP, not just the first', async () => {
    const res = await fetch(proxyUrl)
    expect(res.headers.get('content-security-policy')).toBeTruthy()
  })

  it("denies a geolocation permission request in the live renderer (WebKitGTK deny-all)", async () => {
    await bridgeEval(
      base,
      token,
      `(window.__e2eGeo = null,
        navigator.geolocation.getCurrentPosition(
          () => { window.__e2eGeo = { ok: true } },
          (err) => { window.__e2eGeo = { ok: false, code: err.code, message: err.message } },
        ),
        true)`,
    )

    await browser.waitUntil(
      async () => {
        const result = await bridgeEval(base, token, 'window.__e2eGeo')
        return result.ok && result.result !== null
      },
      { timeout: 15000, interval: 250, timeoutMsg: 'geolocation request never settled' },
    )

    const geo = (await bridgeEval(base, token, 'window.__e2eGeo')).result
    // PERMISSION_DENIED = 1 (the GeolocationPositionError constant) — the
    // request must be actively denied via webview_hardening.rs's handler,
    // not merely unsupported/unavailable (which would also fail, but for
    // the wrong reason and wouldn't prove the handler is wired).
    expect(geo.ok).toBe(false)
    expect(geo.code).toBe(1)
  })
})
