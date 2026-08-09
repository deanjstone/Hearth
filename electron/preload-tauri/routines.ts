// window.hearth.routines's Tauri shim, backed by
// src-tauri/src/routines_commands.rs (Phase 7, tracking issue #27). Note
// the preload method is `remove`, mirroring electron/preload/index.ts's own
// naming even though the Rust/channel-side command is `routines_delete`.

import type { CreateRoutineInput, Routine } from '../shared/protocol.js'
import { onEvent, tauri } from './tauri-global.js'

export const routines = {
  list: (): Promise<Routine[]> => tauri().core.invoke('routines_list') as Promise<Routine[]>,
  create: (input: CreateRoutineInput): Promise<Routine> => tauri().core.invoke('routines_create', { input }) as Promise<Routine>,
  update: (id: string, patch: Partial<CreateRoutineInput>): Promise<Routine | null> =>
    tauri().core.invoke('routines_update', { id, patch }) as Promise<Routine | null>,
  setEnabled: (id: string, enabled: boolean): Promise<Routine | null> =>
    tauri().core.invoke('routines_set_enabled', { id, enabled }) as Promise<Routine | null>,
  remove: (id: string): Promise<void> => tauri().core.invoke('routines_delete', { id }) as Promise<void>,
  runNow: (id: string): Promise<void> => tauri().core.invoke('routines_run_now', { id }) as Promise<void>,
  onDue: (cb: (r: Routine) => void): (() => void) => onEvent<Routine>('routines:due', cb),
}
