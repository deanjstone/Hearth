// window.hearth.win's Tauri shim, backed by src-tauri/src/misc_commands.rs
// (Phase 7, tracking issue #27). `send` rather than `invoke`+await: the
// Electron original is fire-and-forget (`ipcRenderer.on`, no return value).

import { tauri } from './tauri-global.js'

export const win = {
  zoomToggle: (): void => void tauri().core.invoke('window_zoom_toggle'),
}
