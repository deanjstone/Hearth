// Assembles `window.hearth` for the Tauri build, side-effect import from
// main.tsx. Electron already gets `window.hearth` for free via
// contextBridge (electron/preload/index.ts) — this is the Tauri-only
// equivalent, backed by the electron/preload-tauri/* shims that translate
// each call into a real Tauri `invoke`/event.
//
// Deliberately partial: Phase 1 (Hearth#27) ported `selfMod` + `frontendReady`;
// Phase 2 adds `view.onNavigate` (the agent's route-capture bridge — see
// src-tauri/src/bridge.rs); Phase 3 Chunk 4 adds `agentRuntime` (the startup
// Node/adapter check — see src-tauri/src/agents_commands.rs); Phase 3 Chunk 5
// adds `agent`/`permission`/`auth` (the full agent-chat surface — see
// src-tauri/src/agent_commands.rs). Not the full ~90-method/20-namespace
// `HearthApi` surface (see spec #26's "IPC surface" decision — ported per
// in-scope subsystem as it lands, not upfront; `window.hearth.sessions.*`
// still isn't ported, so the renderer's full chat flow can't run end-to-end
// under Tauri yet even with `agent` now wired). The cast below is
// intentionally unsound today and will narrow to a real subset type, or fill
// in for real, as later phases wire their own namespaces through IPC.
//
// No-ops entirely under Electron (`window.__TAURI__` absent) so this import
// is safe regardless of which shell the renderer is actually running under.

import type { HearthApi } from '../../electron/preload/index.js'
import { agent } from '../../electron/preload-tauri/agent.js'
import { agentRuntime } from '../../electron/preload-tauri/agent-runtime.js'
import { auth } from '../../electron/preload-tauri/auth.js'
import { permission } from '../../electron/preload-tauri/permission.js'
import { selfMod } from '../../electron/preload-tauri/self-mod.js'
import { view } from '../../electron/preload-tauri/view.js'

declare global {
  interface Window {
    __TAURI__?: unknown
  }
}

if (typeof window !== 'undefined' && window.__TAURI__ && !window.hearth) {
  window.hearth = { selfMod, view, agentRuntime, agent, permission, auth } as unknown as HearthApi
}
