// Assembles `window.hearth` for the Tauri build, side-effect import from
// main.tsx. Electron already gets `window.hearth` for free via
// contextBridge (electron/preload/index.ts) — this is the Tauri-only
// equivalent, backed by the electron/preload-tauri/* shims that translate
// each call into a real Tauri `invoke`/event.
//
// Deliberately partial: Phase 1 (Hearth#27) ported `selfMod` + `frontendReady`;
// Phase 2 adds `view.onNavigate` (the agent's route-capture bridge — see
// src-tauri/src/bridge.rs). Not the full ~90-method/20-namespace `HearthApi`
// surface (see spec #26's "IPC surface" decision — ported per in-scope
// subsystem as it lands, not upfront). The cast below is intentionally
// unsound today and will narrow to a real subset type, or fill in for real,
// as later phases wire their own namespaces through IPC.
//
// No-ops entirely under Electron (`window.__TAURI__` absent) so this import
// is safe regardless of which shell the renderer is actually running under.

import type { HearthApi } from '../../electron/preload/index.js'
import { selfMod } from '../../electron/preload-tauri/self-mod.js'
import { view } from '../../electron/preload-tauri/view.js'

declare global {
  interface Window {
    __TAURI__?: unknown
  }
}

if (typeof window !== 'undefined' && window.__TAURI__ && !window.hearth) {
  window.hearth = { selfMod, view } as unknown as HearthApi
}
