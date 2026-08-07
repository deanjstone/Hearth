// window.hearth.about's Tauri shim, backed by src-tauri/src/misc_commands.rs
// (Phase 7, tracking issue #27). `electron`/`node` versions have no Tauri
// equivalent (this shell has neither) — reported as `null`, not omitted, so
// the About panel's rendering doesn't need a shell-shaped branch.

import { tauri } from './tauri-global.js'

export const about = {
  info: (): Promise<{
    app: string
    electron: string | null
    node: string | null
    tauri: string
    acpSdk: string | null
    claudeAdapter: string | null
    codexAdapter: string | null
  }> =>
    tauri()
      .core.invoke('about_info')
      .then((info) => ({ electron: null, node: null, ...(info as object) })) as Promise<{
      app: string
      electron: string | null
      node: string | null
      tauri: string
      acpSdk: string | null
      claudeAdapter: string | null
      codexAdapter: string | null
    }>,
}
