import { resolve } from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwind from '@tailwindcss/vite'
import { tanstackRouter } from '@tanstack/router-plugin/vite'
import { selfModOverlay } from './electron/vite-plugins/self-mod-overlay'

// Standalone renderer-only dev server for the Tauri build. `src-tauri/tauri.conf.json`
// has no `beforeDevCommand` — the window URL is a hardcoded `http://localhost:5173`
// with no automatic frontend startup (electron-vite's `dev` script launches Electron +
// Vite together; there is no Tauri equivalent yet). This config is the renderer half
// of electron.vite.config.ts, extracted to run standalone via plain `vite` on a fixed
// port so `cargo tauri dev`/`build` and the e2e-tests/ CI suite (Hearth#27 Phase 1) have
// something to point at.
export default defineConfig({
  root: '.',
  resolve: {
    alias: { '@': resolve(__dirname, 'src') },
  },
  server: {
    port: 5173,
    strictPort: true,
    hmr: { overlay: false },
  },
  plugins: [selfModOverlay(resolve(__dirname)), tanstackRouter({ target: 'react', routesDirectory: 'src/routes' }), react(), tailwind()],
  build: {
    rollupOptions: {
      input: {
        index: resolve(__dirname, 'index.html'),
        overlay: resolve(__dirname, 'overlay.html'),
      },
    },
  },
})
