// window.hearth.data's Tauri shim, backed by src-tauri/src/misc_commands.rs
// (Phase 7, tracking issue #27).

import { tauri } from './tauri-global.js'

export const data = {
  reveal: (): Promise<void> => tauri().core.invoke('data_reveal') as Promise<void>,
  revealLogs: (): Promise<void> => tauri().core.invoke('logs_reveal') as Promise<void>,
}
