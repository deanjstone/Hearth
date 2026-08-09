// window.hearth.auth's Tauri shim, backed by the real Tauri commands built in
// Chunk 5 (src-tauri/src/agent_commands.rs, spec deanjstone/Hearth#48).
// Mirrors electron/preload/index.ts's `auth` object, simplified for this
// phase's subscription-only scope (spec #48's Out of Scope — no api-key
// path) — `logout` never returns `cleared: true` here, since there's no
// stored secret on the Tauri side to clear.

import type { AgentKind, AuthState } from '../shared/protocol.js'
import { tauri } from './tauri-global.js'

export const auth = {
  status: (kind: AgentKind, reconnect?: boolean): Promise<AuthState> =>
    tauri().core.invoke('auth_status', { kind, reconnect }) as Promise<AuthState>,
  login: (kind: AgentKind): Promise<{ command: string }> =>
    tauri().core.invoke('auth_login', { kind }) as Promise<{ command: string }>,
  logout: (kind: AgentKind): Promise<{ cleared?: boolean; command?: string }> =>
    tauri().core.invoke('auth_logout', { kind }) as Promise<{ cleared?: boolean; command?: string }>,
  // No Tauri-side auth:changed emission yet: Electron only fires it from the
  // api-key clear-secret branch, which has no Rust equivalent this phase.
  // Stubbed (not omitted) so callers don't need to branch on which backend
  // they're running against — matches self-mod.ts's onActivity/onValidation.
  onChanged: (_cb: (state: AuthState) => void): (() => void) => () => {},
}
