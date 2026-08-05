// window.hearth.permission's Tauri shim, backed by the real Tauri commands
// built in Chunk 5 (src-tauri/src/agent_commands.rs, spec deanjstone/Hearth#48).
// Mirrors electron/preload/index.ts's `permission` object.

import type { PermissionRequestPayload } from '../shared/protocol.js'
import { onEvent, tauri } from './tauri-global.js'

export const permission = {
  onRequest: (cb: (payload: PermissionRequestPayload) => void) =>
    onEvent<PermissionRequestPayload>('permission:request', cb),
  // Fire-and-forget, matching Electron's `ipcRenderer.send` (main holds the
  // resolver) — the caller never awaits this.
  respond: (id: string, optionId: string): void => {
    void tauri().core.invoke('permission_respond', { id, optionId })
  },
}
