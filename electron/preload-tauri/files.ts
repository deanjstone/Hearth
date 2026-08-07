// window.hearth.files's Tauri shim, backed by src-tauri/src/fs_commands.rs
// (Phase 7, tracking issue #27).

import { tauri } from './tauri-global.js'

export interface FileEntry {
  name: string
  rel: string
  dir: boolean
}

export interface FileContent {
  rel: string
  content: string
  readonly: boolean
}

export const files = {
  list: (cwd: string | undefined, rel?: string): Promise<FileEntry[]> =>
    tauri().core.invoke('fs_list', { cwd, rel }) as Promise<FileEntry[]>,
  read: (cwd: string | undefined, rel: string): Promise<FileContent> =>
    tauri().core.invoke('fs_read', { cwd, rel }) as Promise<FileContent>,
  write: (cwd: string | undefined, rel: string, content: string): Promise<void> =>
    tauri().core.invoke('fs_write', { cwd, rel, content }) as Promise<void>,
}
