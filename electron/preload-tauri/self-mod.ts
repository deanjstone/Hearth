// JS-side compatibility shim for `window.hearth.selfMod.*`, backed by real
// Tauri IPC commands (src-tauri/src/selfmod_commands.rs) instead of
// Electron's `ipcRenderer`. Exposes the same shape electron/preload/index.ts's
// `selfMod` object does — see self-mod-service.ts's `StepResult` and
// git.ts's `SelfModLogEntry` — so renderer call sites don't need to branch
// on which backend they're running against.
//
// Uses the `window.__TAURI__` global that Tauri injects when
// `withGlobalTauri: true` (tauri.conf.json) is set, rather than the
// `@tauri-apps/api` npm package — keeps this shim dependency-free until a
// later chunk's preload surface needs more of that package.
//
// `onActivity`/`onValidation` are stubbed no-ops for now: nothing emits
// those events yet on the Rust side — that needs the turn coordinator wired
// through IPC, which needs a real `AgentHost` (Phase 3, ACP agent runtime).
// They're kept in the same shape so call sites don't need conditional logic
// once this shim starts backing them for real.

export type SelfModKind = 'code' | 'soul' | 'memory'
export type ReloadKind = 'hmr' | 'full-reload' | 'process-restart'

export interface SelfModLogEntry {
  hash: string
  subject: string
  conversationId: string | null
  kind: SelfModKind
  runId: string | null
  subagent: string | null
  reverted: boolean
}

export type StepResult =
  | { status: 'ok'; commit: string; changedPaths: string[]; reload: ReloadKind }
  | { status: 'dirty' }
  | { status: 'conflict'; hash: string; files: string[] }
  | { status: 'noop' }

export interface TypecheckResult {
  ok: boolean
  output: string
}

interface TauriGlobal {
  core: {
    invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>
  }
}

function tauri(): TauriGlobal {
  const g = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__
  if (!g) {
    throw new Error('preload-tauri/self-mod: window.__TAURI__ is not present (is withGlobalTauri enabled?)')
  }
  return g
}

export const selfMod = {
  history: (): Promise<SelfModLogEntry[]> => tauri().core.invoke('self_mod_history') as Promise<SelfModLogEntry[]>,
  undo: (hash: string): Promise<StepResult> => tauri().core.invoke('self_mod_undo', { hash }) as Promise<StepResult>,
  redo: (hash: string): Promise<StepResult> => tauri().core.invoke('self_mod_redo', { hash }) as Promise<StepResult>,
  onActivity: (_cb: (activity: unknown) => void): (() => void) => () => {},
  onValidation: (_cb: (result: TypecheckResult) => void): (() => void) => () => {},
}
