// Standalone Vite dev server for the Tauri/WebKitGTK HMR spike
// (docs/decisions/rust-tauri-feasibility.md §10.1). Reuses the exact same
// renderer plugins as electron.vite.config.ts so the server under test is
// functionally identical to what Electron loads today — the only variable
// under test is which webview (Chromium vs WebKitGTK) points at it.
//
// Run: pnpm exec vite --config spike/vite.config.spike.ts
// Not part of the real app build — spike-only, gitignored build output.

import { resolve } from 'node:path'
import type { Plugin } from 'vite'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwind from '@tailwindcss/vite'
import { tanstackRouter } from '@tanstack/router-plugin/vite'
import { selfModOverlay } from '../electron/vite-plugins/self-mod-overlay'

const repoRoot = resolve(__dirname, '..')

interface MarkerReport {
  component: string
  value: number
  host: 'electron' | 'tauri' | 'unknown'
  ts: number
}

// Dev-only middleware so spike/run-sequence.mjs can observe what actually
// rendered without an Electron-style capturePage()/executeJavaScript bridge —
// same-origin with the app, so it isn't affected by index.html's CSP meta tag.
function spikeMarkerEndpoint(): Plugin {
  const log: MarkerReport[] = []
  return {
    name: 'spike:marker-endpoint',
    apply: 'serve',
    configureServer(server) {
      server.middlewares.use('/__spike/marker', (req, res) => {
        if (req.method === 'POST') {
          const chunks: Buffer[] = []
          req.on('data', (c) => chunks.push(c))
          req.on('end', () => {
            try {
              const body = JSON.parse(Buffer.concat(chunks).toString('utf8')) as Partial<MarkerReport>
              if (typeof body.component === 'string' && typeof body.value === 'number') {
                log.push({
                  component: body.component,
                  value: body.value,
                  host: body.host === 'electron' || body.host === 'tauri' ? body.host : 'unknown',
                  ts: Date.now(),
                })
              }
            } catch {
              // ignore malformed reports
            }
            res.statusCode = 204
            res.end()
          })
          return
        }
        res.statusCode = 200
        res.setHeader('content-type', 'application/json')
        res.end(JSON.stringify(log))
      })
    },
  }
}

export default defineConfig({
  // Relative to this config file's own location (spike/) per Vite's root
  // resolution rules — go up to the real repo root, where index.html/src live.
  root: repoRoot,
  resolve: {
    alias: { '@': resolve(repoRoot, 'src') },
  },
  server: {
    port: 5183,
    strictPort: true,
    hmr: { overlay: false },
    // root is the whole repo, so without this, editing spike-only files (this
    // config, run-sequence.mjs, the Rust src-tauri/ tree) triggers Vite's
    // default full-reload-on-unwatched-file-change — noise that would corrupt
    // the "unchanged" assertions in run-sequence.mjs's own test sequence.
    watch: { ignored: ['**/spike/**'] },
  },
  plugins: [
    selfModOverlay(repoRoot),
    tanstackRouter({ target: 'react', routesDirectory: 'src/routes' }),
    react(),
    tailwind(),
    spikeMarkerEndpoint(),
  ],
})
