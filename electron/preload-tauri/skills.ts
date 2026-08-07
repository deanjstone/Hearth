// window.hearth.skills's Tauri shim, backed by src-tauri/src/skills_commands.rs
// (Phase 7, tracking issue #27).

import type { AvailableCommand } from '../shared/protocol.js'
import { tauri } from './tauri-global.js'

export interface SkillInfo {
  name: string
  description: string
  scope: 'global' | 'workspace'
  path: string
  enabled: boolean
}

export const skills = {
  list: (cwd?: string): Promise<{ skills: SkillInfo[]; commands: AvailableCommand[] }> =>
    tauri().core.invoke('skills_list', { cwd }) as Promise<{ skills: SkillInfo[]; commands: AvailableCommand[] }>,
  reveal: (): Promise<void> => tauri().core.invoke('skills_reveal') as Promise<void>,
  setEnabled: (path: string, enabled: boolean): Promise<string> =>
    tauri().core.invoke('skills_set_enabled', { path, enabled }) as Promise<string>,
}
