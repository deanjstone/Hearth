import { resolve } from 'node:path'

// Drives the real Tauri build via @wdio/tauri-service (the current
// Tauri-docs-recommended pattern — see Hearth#37's resolution). The service
// owns tauri-driver's lifecycle; this config only needs to point it at the
// debug binary. Requires:
//   - the frontend already serving on :5173 (`pnpm run dev:tauri-frontend`
//     at the repo root — this app has no beforeDevCommand/frontendDist, see
//     vite.config.tauri.ts's comment)
//   - a debug build already produced (`cargo tauri build --debug --no-bundle`
//     in src-tauri/)
// Both are separate CI steps (see .github/workflows/ci.yml), not driven from
// here, so local/CI timing is identical and each step's failure is legible
// on its own.
const APPLICATION = resolve(import.meta.dirname, '../src-tauri/target/debug/hearth')

export const config = {
  runner: 'local',
  specs: ['./specs/**/*.spec.js'],
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
  mochaOpts: {
    ui: 'bdd',
    timeout: 60000,
  },
}
