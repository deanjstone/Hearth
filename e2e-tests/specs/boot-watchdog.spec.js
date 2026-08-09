// Boot watchdog revert (Hearth#37's resolution): a fresh app process is
// spawned by wdio.boot-watchdog.conf.js's own session, with a bricking
// marker already armed (see that config's onPrepare) naming a real fixture
// commit. lib.rs's setup() hook runs inspect_boot() before the window loads
// and, finding the marker, reverts that commit — this test just needs to
// confirm the revert landed and the app booted clean afterward.

import { execFileSync } from 'node:child_process'
import { existsSync, readFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { resolve } from 'node:path'
import { expect } from '@wdio/globals'

// onPrepare (launcher process) hands this off via a scratch file since it
// runs in a different process than this spec's worker.
const { originalSource, probeAbs, repoRoot } = JSON.parse(
  readFileSync(resolve(import.meta.dirname, '../.boot-watchdog-fixture.json'), 'utf8'),
)

function git(...args) {
  return execFileSync('git', args, { cwd: repoRoot, encoding: 'utf8' }).trim()
}

describe('boot watchdog revert', () => {
  it('reverts the bricking commit before the window loads', () => {
    // The revert already happened in setup(), before this session's window
    // even started — by the time WebDriver can talk to the app, it's done.
    const log = git('log', '--oneline', '-3')
    expect(log).toContain('Revert')
    expect(readFileSync(probeAbs, 'utf8')).toBe(originalSource)
  })

  it('boots clean afterward — the window renders and frontend_ready cleared the marker', async () => {
    const root = await $('#root')
    await expect(root).toExist()

    // frontend_ready() (src/lib/FrontendReadySignal.tsx) fires once mounted
    // and clears the marker via boot_watchdog.confirm_ready() — if the app
    // were stuck/crash-looping, the marker would still be present.
    const appDataDir = resolve(process.env.XDG_DATA_HOME || resolve(homedir(), '.local/share'), 'ai.hearth.dev')
    const markerPath = resolve(appDataDir, 'pending-self-mod-restart.json')
    expect(existsSync(markerPath)).toBe(false)
  })
})
