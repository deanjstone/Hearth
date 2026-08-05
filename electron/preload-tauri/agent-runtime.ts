// JS-side shim for `window.hearth.agentRuntime.*`, backed by the real Tauri
// commands `agent_runtime_status`/`agent_runtime_recheck`
// (src-tauri/src/agents_commands.rs). Unlike Electron — which spawns
// adapters via ELECTRON_RUN_AS_NODE and so never fails this check —
// the Tauri build genuinely needs a system-installed Node.js (spec #26's
// Implementation Decisions), so this is the one `window.hearth` namespace
// with no constant Electron equivalent to fall back to; see
// electron/preload/index.ts's `agentRuntime` for that always-`ok` shape.
//
// Uses the `window.__TAURI__` global the same way self-mod.ts/ready.ts do —
// see self-mod.ts's doc comment for why (dependency-free until a later
// chunk's preload surface needs more of `@tauri-apps/api`).

import type { AgentRuntimeStatus } from '../shared/protocol.js'

interface TauriGlobal {
  core: {
    invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>
  }
}

function tauri(): TauriGlobal {
  const g = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__
  if (!g) {
    throw new Error('preload-tauri/agent-runtime: window.__TAURI__ is not present (is withGlobalTauri enabled?)')
  }
  return g
}

export const agentRuntime = {
  status: (): Promise<AgentRuntimeStatus> =>
    tauri().core.invoke('agent_runtime_status') as Promise<AgentRuntimeStatus>,
  recheck: (): Promise<AgentRuntimeStatus> =>
    tauri().core.invoke('agent_runtime_recheck') as Promise<AgentRuntimeStatus>,
}
