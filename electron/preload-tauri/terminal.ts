// window.hearth.terminal's Tauri shim, backed by the real Tauri commands
// built for Phase 4 (tracking issue #27: src-tauri/src/terminal_commands.rs).
// Mirrors electron/preload/index.ts's `terminal` object method-for-method —
// including its void-returning create/write/resize/kill (Electron's are
// `ipcRenderer.send`, fire-and-forget; the `invoke` calls here are too, via
// `void`, so this shim's exported shape stays parity with `HearthApi`'s).

import { onEvent, tauri } from './tauri-global.js'

export const terminal = {
  create: (id: string, cwd: string | undefined, cols: number, rows: number): void => {
    void tauri().core.invoke('terminal_create', { id, cwd, cols, rows })
  },
  write: (id: string, data: string): void => {
    void tauri().core.invoke('terminal_write', { id, data })
  },
  resize: (id: string, cols: number, rows: number): void => {
    void tauri().core.invoke('terminal_resize', { id, cols, rows })
  },
  kill: (id: string): void => {
    void tauri().core.invoke('terminal_kill', { id })
  },
  onData: (cb: (id: string, data: string) => void) =>
    onEvent<{ id: string; data: string }>('terminal:data', (p) => cb(p.id, p.data)),
  onExit: (cb: (id: string, reason?: string) => void) =>
    onEvent<{ id: string; reason?: string }>('terminal:exit', (p) => cb(p.id, p.reason)),
}
