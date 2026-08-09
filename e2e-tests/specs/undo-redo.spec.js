// Undo/redo via the real window.hearth.selfMod.{undo,redo} IPC (the one part
// of Phase 1's pin/edit/apply/turn-end/undo/redo sequence with an actual
// frontend command today — see Hearth#37's resolution).
//
// SelfModService::undo/redo act on git commits carrying Hearth-* trailers
// (git.rs's commit_self_mod format). There's no IPC path yet to CREATE such a
// commit (commit_managed/turn_coordinator aren't wired to IPC — Phase 3), so
// this test creates its fixture commit directly via git (test-only setup,
// not the thing under test) and then drives undo/redo through the real IPC
// surface, which is the thing under test.
//
// Mutates real git history in whatever checkout it runs against — safe in
// CI's ephemeral checkout; only ever run this against a throwaway clone or
// worktree locally, never a real working branch.

import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { expect } from '@wdio/globals'

const REPO_ROOT = resolve(import.meta.dirname, '../..')
const PROBE_REL = 'src/__e2e__/selfModProbe.tsx'
const PROBE_ABS = resolve(REPO_ROOT, PROBE_REL)
const SELECTOR = '[data-testid="self-mod-probe"]'

function git(...args) {
  return execFileSync('git', args, { cwd: REPO_ROOT, encoding: 'utf8' }).trim()
}

describe('undo/redo (real IPC)', () => {
  const originalSource = readFileSync(PROBE_ABS, 'utf8')
  let commitHash
  let preTestHead

  before(() => {
    preTestHead = git('rev-parse', 'HEAD')
    const editedSource = originalSource.replace('probe-v1', 'probe-v1-undo-redo-fixture')
    writeFileSync(PROBE_ABS, editedSource)
    git('add', PROBE_REL)
    const message = ['e2e undo/redo fixture', '', 'Hearth-Conversation: e2e-undo-redo', 'Hearth-Kind: code', 'Hearth-SelfMod: true'].join('\n')
    git('commit', '-m', message)
    commitHash = git('rev-parse', 'HEAD')
  })

  after(() => {
    // Reset to the exact pre-test HEAD, not just "back one" — how many
    // commits landed on top depends on how far the test got (fixture only,
    // fixture+undo-revert, or fixture+undo-revert+redo-revert).
    try {
      git('reset', '--hard', preTestHead)
    } catch {
      // Best-effort — leave state for manual inspection if this fails.
    }
    writeFileSync(PROBE_ABS, originalSource)
  })

  it('undoes a self-mod commit through window.hearth.selfMod.undo, live via HMR', async () => {
    const result = await browser.execute((hash) => window.hearth.selfMod.undo(hash), commitHash)
    expect(result.status).toBe('ok')
    expect(readFileSync(PROBE_ABS, 'utf8')).toBe(originalSource)

    await browser.waitUntil(async () => (await $(SELECTOR).getText()) === 'probe-v1', {
      timeout: 15000,
      timeoutMsg: 'expected the live window to reflect the undo via HMR',
    })
  })

  it('redoes it through window.hearth.selfMod.redo, live via HMR', async () => {
    const result = await browser.execute((hash) => window.hearth.selfMod.redo(hash), commitHash)
    expect(result.status).toBe('ok')
    expect(readFileSync(PROBE_ABS, 'utf8')).toContain('probe-v1-undo-redo-fixture')

    await browser.waitUntil(async () => (await $(SELECTOR).getText()) === 'probe-v1-undo-redo-fixture', {
      timeout: 15000,
      timeoutMsg: 'expected the live window to reflect the redo via HMR',
    })
  })
})
