// HMR self-mod swap, against the real Hearth app + real Vite dev server —
// see Hearth#37's resolution. Mirrors spike/tauri-hmr-check/'s pin ->
// edit-on-disk -> (unchanged) -> apply -> (changed, no full reload) sequence,
// but drives the real src/__e2e__/selfModProbe.tsx probe instead of a
// throwaway marker module, and asserts against the real running window.

import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { expect } from '@wdio/globals'

const OVERLAY_BASE = 'http://localhost:5173'
const PROBE_REL = 'src/__e2e__/selfModProbe.tsx'
const PROBE_ABS = resolve(import.meta.dirname, '../..', PROBE_REL)
const SELECTOR = '[data-testid="self-mod-probe"]'

async function overlay(op, body) {
  const res = await fetch(`${OVERLAY_BASE}/__hearth/self-mod`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ op, ...body }),
  })
  if (!res.ok) throw new Error(`self-mod ${op} failed: ${res.status}`)
  return res.json()
}

describe('HMR self-mod swap', () => {
  const originalSource = readFileSync(PROBE_ABS, 'utf8')

  afterEach(() => {
    // Best-effort cleanup: always restore the on-disk probe, whether the test
    // passed or failed mid-sequence.
    writeFileSync(PROBE_ABS, originalSource)
  })

  it('suppresses the live update while pinned, then applies via targeted HMR', async () => {
    const preEditText = await $(SELECTOR).getText()

    // A marker that only survives a targeted HMR swap, not a full page reload.
    await browser.execute(() => {
      window.__hearthE2eMarker = 'still-here'
    })

    // 1. Pin the current (pre-edit) content as the baseline to keep serving.
    await overlay('pin', { path: PROBE_REL, baseline: originalSource })

    // 2. Edit on disk. The pin should suppress this from reaching the live DOM.
    const editedSource = originalSource.replace(preEditText, `${preEditText}-edited`)
    writeFileSync(PROBE_ABS, editedSource)
    await browser.pause(500) // let Vite's file watcher + HMR pipeline settle

    expect(await $(SELECTOR).getText()).toBe(preEditText)

    // 3. Apply — un-pins and triggers the real HMR swap.
    await overlay('apply', { paths: [PROBE_REL] })
    await browser.waitUntil(async () => (await $(SELECTOR).getText()) !== preEditText, {
      timeout: 20000,
      interval: 250,
      timeoutMsg: `expected ${SELECTOR} to update after apply`,
    })

    expect(await $(SELECTOR).getText()).toBe(`${preEditText}-edited`)

    // 4. No full page reload happened — the pre-apply marker survived.
    const marker = await browser.execute(() => window.__hearthE2eMarker)
    expect(marker).toBe('still-here')
  })
})
