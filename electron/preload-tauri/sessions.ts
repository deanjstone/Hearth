// window.hearth.sessions's Tauri shim, backed by the real Tauri commands
// built to close Phase 3's exit-criterion gap (src-tauri/src/sessions_commands.rs,
// spec deanjstone/Hearth#48: "chat with Claude/Codex works through the Tauri
// build") — ChatView.tsx/sessions.ts's `ensureActiveSession()` needs this
// before `agent_prompt` is even reachable. Mirrors
// electron/preload/index.ts's `sessions` object method-for-method.

import type { CreateSessionInput, SessionDetail, SessionMeta, SessionSearchHit, TranscriptEntry } from '../main/sessions/store.js'
import type { WorkspaceKind } from '../shared/protocol.js'
import { tauri } from './tauri-global.js'

export const sessions = {
  list: (): Promise<SessionMeta[]> => tauri().core.invoke('sessions_list') as Promise<SessionMeta[]>,
  search: (query: string): Promise<SessionSearchHit[]> =>
    tauri().core.invoke('sessions_search', { query }) as Promise<SessionSearchHit[]>,
  create: (input: CreateSessionInput): Promise<SessionMeta> =>
    tauri().core.invoke('sessions_create', { input }) as Promise<SessionMeta>,
  get: (id: string): Promise<SessionDetail | null> => tauri().core.invoke('sessions_get', { id }) as Promise<SessionDetail | null>,
  append: (id: string, entries: TranscriptEntry[]): Promise<void> =>
    tauri().core.invoke('sessions_append', { id, entries }) as Promise<void>,
  rename: (id: string, title: string): Promise<SessionMeta | null> =>
    tauri().core.invoke('sessions_rename', { id, title }) as Promise<SessionMeta | null>,
  setKind: (id: string, kind: WorkspaceKind): Promise<SessionMeta | null> =>
    tauri().core.invoke('sessions_set_kind', { id, kind }) as Promise<SessionMeta | null>,
  archive: (id: string): Promise<void> => tauri().core.invoke('sessions_archive', { id }) as Promise<void>,
  delete: (id: string): Promise<void> => tauri().core.invoke('sessions_delete', { id }) as Promise<void>,
  duplicate: (id: string): Promise<SessionMeta | null> =>
    tauri().core.invoke('sessions_duplicate', { id }) as Promise<SessionMeta | null>,
}
