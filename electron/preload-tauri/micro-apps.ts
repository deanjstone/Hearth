// window.hearth.microApps's Tauri shim, backed by the real Tauri commands
// built for Phase 6 (tracking issue #27: src-tauri/src/micro_apps_commands.rs).
// Mirrors electron/preload/index.ts's `microApps` object method-for-method.

import type { AppCapabilities } from '../main/micro-apps/capabilities.js'
import type { MicroAppInfo } from '../main/micro-apps/server.js'
import type { ScaffoldResult, StarterInfo } from '../main/micro-apps/scaffold.js'
import { tauri } from './tauri-global.js'

export const microApps = {
  create: (name: string, starter?: string): Promise<ScaffoldResult> =>
    tauri().core.invoke('micro_app_create', { name, starter }) as Promise<ScaffoldResult>,
  list: (): Promise<MicroAppInfo[]> => tauri().core.invoke('micro_app_list') as Promise<MicroAppInfo[]>,
  starters: (): Promise<StarterInfo[]> => tauri().core.invoke('micro_app_starters') as Promise<StarterInfo[]>,
  start: (name: string): Promise<string> => tauri().core.invoke('micro_app_start', { name }) as Promise<string>,
  stop: (name: string): Promise<void> => tauri().core.invoke('micro_app_stop', { name }) as Promise<void>,
  // W6 egress grants: read approved + pending hosts, approve/revoke per app.
  capabilities: (name: string): Promise<AppCapabilities> =>
    tauri().core.invoke('micro_app_capabilities', { name }) as Promise<AppCapabilities>,
  approve: (name: string, hosts: string[]): Promise<void> =>
    tauri().core.invoke('micro_app_approve', { name, hosts }) as Promise<void>,
  revoke: (name: string, host?: string): Promise<void> =>
    tauri().core.invoke('micro_app_revoke', { name, host }) as Promise<void>,
}
