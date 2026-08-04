// Separate wdio config for the boot-watchdog revert test (Hearth#37's
// resolution). BootWatchdog.inspect_boot() runs at app *startup*, inside
// lib.rs's setup() hook, before the window ever loads — so the only way to
// test it is to have the marker file already in place *before* the app
// process is spawned. WebDriverIO/tauri-service spawn the app as part of
// session creation, which happens before any per-test hook runs, so the
// marker has to be armed in `onPrepare` (runs once, before any session).
//
// Kept in its own config/session (not sharing wdio.conf.js's run) so this
// doesn't force a bricked-boot marker onto the HMR/undo-redo sessions too —
// every @wdio/tauri-service session launches a fresh app process, and
// inspect_boot() would trip on any of them if the marker were armed globally.

import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { dirname, resolve } from 'node:path'

const REPO_ROOT = resolve(import.meta.dirname, '..')
const PROBE_REL = 'src/__e2e__/selfModProbe.tsx'
const PROBE_ABS = resolve(REPO_ROOT, PROBE_REL)
const APPLICATION = resolve(REPO_ROOT, 'src-tauri/target/debug/hearth')

// tauri::Manager::path().app_data_dir() = dirs::data_dir()/<identifier> — on
// Linux, $XDG_DATA_HOME (or ~/.local/share)/<identifier>. Identifier is
// src-tauri/tauri.conf.json's "ai.hearth.dev".
const APP_DATA_DIR = resolve(process.env.XDG_DATA_HOME || resolve(homedir(), '.local/share'), 'ai.hearth.dev')
const MARKER_PATH = resolve(APP_DATA_DIR, 'pending-self-mod-restart.json')
// onPrepare runs in the launcher process; each spec file runs in its own
// worker process (WebdriverIO's local runner), so state can't cross via
// globalThis/module scope — hand it off through a scratch file instead.
const FIXTURE_HANDOFF_PATH = resolve(import.meta.dirname, '.boot-watchdog-fixture.json')

function git(...args) {
  return execFileSync('git', args, { cwd: REPO_ROOT, encoding: 'utf8' }).trim()
}

let originalSource
let preTestHead

export const config = {
  runner: 'local',
  specs: ['./specs/boot-watchdog.spec.js'],
  maxInstances: 1,
  capabilities: [
    {
      platformName: 'linux',
      maxInstances: 1,
      'tauri:options': { application: APPLICATION },
    },
  ],
  services: [['@wdio/tauri-service', {}]],
  logLevel: 'info',
  framework: 'mocha',
  reporters: ['spec'],
  mochaOpts: { ui: 'bdd', timeout: 60000 },

  onPrepare: () => {
    preTestHead = git('rev-parse', 'HEAD')
    originalSource = readFileSync(PROBE_ABS, 'utf8')
    writeFileSync(PROBE_ABS, originalSource.replace('probe-v1', 'probe-v1-boot-watchdog-fixture'))
    git('add', PROBE_REL)
    const message = ['e2e boot-watchdog fixture', '', 'Hearth-Conversation: e2e-boot-watchdog', 'Hearth-Kind: code', 'Hearth-SelfMod: true'].join('\n')
    git('commit', '-m', message)
    const commit = git('rev-parse', 'HEAD')

    if (!existsSync(APP_DATA_DIR)) mkdirSync(APP_DATA_DIR, { recursive: true })
    if (!existsSync(dirname(MARKER_PATH))) mkdirSync(dirname(MARKER_PATH), { recursive: true })
    writeFileSync(
      MARKER_PATH,
      JSON.stringify({ commit, armedAt: new Date().toISOString(), attempts: 0 }),
    )

    writeFileSync(FIXTURE_HANDOFF_PATH, JSON.stringify({ commit, originalSource, probeAbs: PROBE_ABS, repoRoot: REPO_ROOT }))
  },

  onComplete: () => {
    // Reset to the exact pre-test HEAD, not just "back one" — a successful
    // boot-watchdog revert adds a second commit (the revert itself) on top
    // of the fixture commit, so how many commits landed depends on whether
    // the test passed.
    if (preTestHead) {
      try {
        execFileSync('git', ['reset', '--hard', preTestHead], { cwd: REPO_ROOT })
      } catch {
        // Best-effort — leave state for manual inspection if this fails.
      }
    }
    if (originalSource !== undefined) writeFileSync(PROBE_ABS, originalSource)
    if (existsSync(FIXTURE_HANDOFF_PATH)) rmSync(FIXTURE_HANDOFF_PATH)
  },
}
