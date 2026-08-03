#!/usr/bin/env node
// Drives the Tauri/WebKitGTK HMR spike (docs/decisions/rust-tauri-feasibility.md
// §10.1) against a webview that already has the spike Vite server
// (spike/vite.config.spike.ts, http://localhost:5183) loaded and holding an
// HMR websocket connection.
//
// Usage: node spike/run-sequence.mjs --host=electron   (control)
//        node spike/run-sequence.mjs --host=tauri       (test)
//
// Exercises: pin -> edit-on-disk -> (unchanged) -> apply -> (changed, no
// remount) -> full-reload-during-turn (suppressed) -> turn-end ->
// full-reload-after-turn (not suppressed). See the plan's pass/fail table
// for what each assertion proves.

import { readFileSync, writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const repoRoot = path.resolve(__dirname, '..')
const BASE = 'http://localhost:5183'

const host = (process.argv.find((a) => a.startsWith('--host=')) ?? '').split('=')[1]
if (host !== 'electron' && host !== 'tauri') {
  console.error('Usage: node spike/run-sequence.mjs --host=electron|tauri')
  process.exit(2)
}

const MARKER_REL = 'src/shell/SpikeMarker.ts'
const MARKER_ABS = path.join(repoRoot, MARKER_REL)
const INDEX_ABS = path.join(repoRoot, 'index.html')

const results = []
function record(name, pass, detail) {
  results.push({ name, pass, detail })
  console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name}${detail ? ' — ' + detail : ''}`)
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

async function selfMod(op, extra) {
  const res = await fetch(`${BASE}/__hearth/self-mod`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ op, ...extra }),
  })
  if (!res.ok) throw new Error(`self-mod ${op} failed: ${res.status}`)
}

async function readMarkerState() {
  const res = await fetch(`${BASE}/__spike/marker`)
  const log = await res.json()
  const forHost = log.filter((e) => e.host === host)
  const moduleEntries = forHost.filter((e) => e.component === 'module-counter')
  const rootEntries = forHost.filter((e) => e.component === 'root-mount')
  return {
    moduleValue: moduleEntries.length ? moduleEntries[moduleEntries.length - 1].value : null,
    rootMountCount: rootEntries.length,
  }
}

async function pollUntil(predicate, { timeoutMs = 3000, intervalMs = 150 } = {}) {
  const deadline = Date.now() + timeoutMs
  let last
  while (Date.now() < deadline) {
    last = await readMarkerState()
    if (predicate(last)) return { ok: true, state: last }
    await sleep(intervalMs)
  }
  return { ok: false, state: last }
}

function readModuleCounter(content) {
  const m = content.match(/const MODULE_COUNTER = (\d+)/)
  if (!m) throw new Error(`MODULE_COUNTER literal not found in ${MARKER_REL}`)
  return Number(m[1])
}

function withModuleCounter(content, value) {
  return content.replace(/const MODULE_COUNTER = \d+/, `const MODULE_COUNTER = ${value}`)
}

function touchIndexHtml(content, tag) {
  return content.includes('</html>')
    ? content.replace('</html>', `<!-- spike:${tag} -->\n</html>`)
    : content + `\n<!-- spike:${tag} -->\n`
}

async function main() {
  console.log(`\n=== Spike sequence: host=${host}, base=${BASE} ===\n`)

  const originalMarkerSrc = readFileSync(MARKER_ABS, 'utf8')
  const originalIndexSrc = readFileSync(INDEX_ABS, 'utf8')
  const originalCounter = readModuleCounter(originalMarkerSrc)

  try {
    // 0. Sanity: HMR socket is up and at least one report has landed for this host.
    const initial = await pollUntil((s) => s.moduleValue !== null, { timeoutMs: 5000 })
    record(
      'websocket / initial marker report reaches server',
      initial.ok,
      initial.ok ? `moduleValue=${initial.state.moduleValue}` : 'no module-counter report seen for this host within 5s',
    )
    if (!initial.ok) {
      console.log('\nAborting remaining steps — base signal never arrived.')
      return
    }

    const preEditState = initial.state
    const bumpedValue = originalCounter + 1

    // 1. pin -> edit-on-disk -> confirm the pin holds (marker unchanged).
    await selfMod('pin', { path: MARKER_REL, baseline: originalMarkerSrc })
    writeFileSync(MARKER_ABS, withModuleCounter(originalMarkerSrc, bumpedValue))
    await sleep(800)
    const pinnedState = await readMarkerState()
    record(
      'pin suppresses the live update (marker still reports old value)',
      pinnedState.moduleValue === preEditState.moduleValue,
      `expected ${preEditState.moduleValue}, got ${pinnedState.moduleValue}`,
    )

    // 2. apply -> confirm the new value lands without a root remount.
    await selfMod('apply', { paths: [MARKER_REL] })
    const applied = await pollUntil((s) => s.moduleValue === bumpedValue, { timeoutMs: 3000 })
    record(
      'apply produces the new value via targeted HMR swap',
      applied.ok,
      applied.ok ? `moduleValue=${applied.state.moduleValue}` : `moduleValue stuck at ${applied.state?.moduleValue}`,
    )
    record(
      'apply does not trigger a full page reload',
      applied.ok && applied.state.rootMountCount === preEditState.rootMountCount,
      `rootMountCount before=${preEditState.rootMountCount} after=${applied.state?.rootMountCount}`,
    )

    const postApplyState = applied.ok ? applied.state : pinnedState

    // 3. full-reload-class change DURING an active turn — must be suppressed.
    await selfMod('turn-start', {})
    writeFileSync(INDEX_ABS, touchIndexHtml(originalIndexSrc, 'during-turn'))
    await sleep(1000)
    const duringTurnState = await readMarkerState()
    record(
      'full-reload during an active turn is suppressed',
      duringTurnState.rootMountCount === postApplyState.rootMountCount,
      `rootMountCount before=${postApplyState.rootMountCount} after=${duringTurnState.rootMountCount}`,
    )

    // 4. turn-end, then the same class of change — must NOT be suppressed.
    await selfMod('turn-end', {})
    writeFileSync(INDEX_ABS, touchIndexHtml(originalIndexSrc, 'after-turn'))
    const afterTurn = await pollUntil((s) => s.rootMountCount > duringTurnState.rootMountCount, { timeoutMs: 5000 })
    record(
      'full-reload after turn-end is not suppressed (suppression is turn-scoped)',
      afterTurn.ok,
      afterTurn.ok
        ? `rootMountCount now=${afterTurn.state.rootMountCount}`
        : `rootMountCount stuck at ${afterTurn.state?.rootMountCount}`,
    )
  } finally {
    // Cleanup: restore both files to their original on-disk content. Best-effort —
    // do not let a cleanup failure mask the results already recorded above.
    try {
      writeFileSync(INDEX_ABS, originalIndexSrc)
      writeFileSync(MARKER_ABS, originalMarkerSrc)
    } catch (err) {
      console.error('Cleanup failed — restore src/shell/SpikeMarker.ts and index.html by hand.', err)
    }
  }

  const outFile = path.join(__dirname, `results-${host}.json`)
  writeFileSync(outFile, JSON.stringify(results, null, 2))
  const failed = results.filter((r) => !r.pass)
  console.log(`\n=== ${results.length - failed.length}/${results.length} passed. Written to ${path.relative(repoRoot, outFile)} ===\n`)
  process.exitCode = failed.length ? 1 : 0
}

main().catch((err) => {
  console.error(err)
  process.exitCode = 1
})
