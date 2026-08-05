// window.hearth.workspaces's Tauri shim — deliberately minimal, mirroring
// src-tauri/src/workspaces_commands.rs's own scope decision (see its header
// comment): only `list()` is backed by a real command, always returning the
// one built-in Hearth workspace. This exists to unblock `sessions.ts`'s
// `ensureActiveSession()`, not to port `electron/main/workspaces/registry.ts`'s
// full CRUD (multiple user-added workspaces, git-status polling) — a
// separate, larger concern spec deanjstone/Hearth#48 doesn't ask for.
// `open`/`remove`/`status` throw a clear "not ported" error rather than
// silently vanishing from the object, so a caller that does need them fails
// loudly instead of hitting "undefined is not a function".

import type { Workspace } from '../main/workspaces/registry.js'
import { tauri } from './tauri-global.js'

function notPorted(method: string): never {
  throw new Error(`preload-tauri/workspaces: ${method} is not ported — only workspaces.list() is (see workspaces_commands.rs)`)
}

export const workspaces = {
  list: (): Promise<Workspace[]> => tauri().core.invoke('workspaces_list') as Promise<Workspace[]>,
  open: (): Promise<Workspace | null> => notPorted('open'),
  remove: (_id: string): Promise<void> => notPorted('remove'),
  status: (_path: string): Promise<{ branch: string | null; dirty: number; ahead: number; behind: number }> =>
    notPorted('status'),
}
