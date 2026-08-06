// window.hearth.mcp's Tauri shim, backed by the real Tauri commands built
// for Phase 5 (tracking issue #27: src-tauri/src/mcp_commands.rs). Mirrors
// electron/preload/index.ts's `mcp` object method-for-method.

import type { ActiveConnectors } from '../shared/protocol.js'
import type { ProbeResult } from '../main/mcp/probe.js'
import type { McpServerConfig, McpServerInput } from '../main/mcp/registry.js'
import { tauri } from './tauri-global.js'

export const mcp = {
  list: (): Promise<McpServerConfig[]> => tauri().core.invoke('mcp_list') as Promise<McpServerConfig[]>,
  add: (input: McpServerInput): Promise<McpServerConfig> =>
    tauri().core.invoke('mcp_add', { input }) as Promise<McpServerConfig>,
  update: (id: string, patch: Partial<McpServerInput>): Promise<McpServerConfig | null> =>
    tauri().core.invoke('mcp_update', { id, patch }) as Promise<McpServerConfig | null>,
  remove: (id: string): Promise<void> => tauri().core.invoke('mcp_remove', { id }) as Promise<void>,
  setEnabled: (id: string, enabled: boolean): Promise<void> =>
    tauri().core.invoke('mcp_set_enabled', { id, enabled }) as Promise<void>,
  test: (id: string): Promise<ProbeResult> => tauri().core.invoke('mcp_test', { id }) as Promise<ProbeResult>,
  /** A2: read-only connectors each backend loads from its own CLI config. */
  active: (cwd?: string): Promise<ActiveConnectors> =>
    tauri().core.invoke('connectors_active', { cwd }) as Promise<ActiveConnectors>,
}
