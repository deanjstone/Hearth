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
// src-tauri/src/agent_commands.rs); closing Phase 3's exit-criterion gap adds
// `sessions` (src-tauri/src/sessions_commands.rs) and a deliberately minimal
// `workspaces` (src-tauri/src/workspaces_commands.rs — see its own header
// comment for why it's not a full registry port) — together these are what
// `ChatView.tsx`'s `ensureActiveSession()` needs, so "chat with Claude/Codex
// works through the Tauri build" (spec #48's Phase 3 exit criterion) is now
// actually reachable end-to-end. Phase 4 (tracking issue #27) adds `terminal`
// (src-tauri/src/terminal_commands.rs), the `TerminalTab.tsx` PTY surface.
// Phase 5 adds `mcp` (src-tauri/src/mcp_commands.rs), the MCP server
// registry + read-only active-connectors view. Not the full ~90-method/20-namespace
// `HearthApi` surface (see spec #26's "IPC surface" decision — ported per
// in-scope subsystem as it lands, not upfront). The cast below is
// intentionally unsound today and will narrow to a real subset type, or fill
// in for real, as later phases wire their own namespaces through IPC.
//
// No-ops entirely under Electron (`window.__TAURI__` absent) so this import
// is safe regardless of which shell the renderer is actually running under.

import type { HearthApi } from '../../electron/preload/index.js'
import { agent } from '../../electron/preload-tauri/agent.js'
import { agentRuntime } from '../../electron/preload-tauri/agent-runtime.js'
import { auth } from '../../electron/preload-tauri/auth.js'
import { mcp } from '../../electron/preload-tauri/mcp.js'
import { permission } from '../../electron/preload-tauri/permission.js'
import { selfMod } from '../../electron/preload-tauri/self-mod.js'
import { sessions } from '../../electron/preload-tauri/sessions.js'
import { terminal } from '../../electron/preload-tauri/terminal.js'
import { view } from '../../electron/preload-tauri/view.js'
import { workspaces } from '../../electron/preload-tauri/workspaces.js'

declare global {
  interface Window {
    __TAURI__?: unknown
  }
}

if (typeof window !== 'undefined' && window.__TAURI__ && !window.hearth) {
  window.hearth = {
    selfMod,
    view,
    agentRuntime,
    agent,
    permission,
    auth,
    sessions,
    workspaces,
    terminal,
    mcp,
  } as unknown as HearthApi
}
