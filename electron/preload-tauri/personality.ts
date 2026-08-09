// window.hearth.personality / window.hearth.memory's Tauri shim, backed by
// src-tauri/src/soul_commands.rs (Phase 7, tracking issue #27).

import { tauri } from './tauri-global.js'

export interface SoulConfig {
  length: 'short' | 'balanced' | 'thorough'
  directness: 'gentle' | 'direct'
  density: 'compact' | 'roomy'
}

export const personality = {
  get: (): Promise<SoulConfig> => tauri().core.invoke('personality_get') as Promise<SoulConfig>,
  set: (config: SoulConfig): Promise<{ commit: string; changedPaths: string[] }> =>
    tauri().core.invoke('personality_set', { config }) as Promise<{ commit: string; changedPaths: string[] }>,
}

export const memory = {
  get: (): Promise<string> => tauri().core.invoke('memory_get') as Promise<string>,
  clear: (): Promise<void> => tauri().core.invoke('memory_clear') as Promise<void>,
}
