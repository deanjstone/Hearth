// window.hearth.update's Tauri stub (Phase 7, tracking issue #27). Auto-update
// (distribution/packaging, `tauri-plugin-updater` wiring) is explicitly out
// of scope for this MVP per spec #26's Out of Scope section — this shell is
// dev-only (`cargo tauri dev`), with no update feed to check. Reports a
// permanent `'unsupported'` state rather than `'idle'`: `UpdateBanner.tsx`
// mounts unconditionally on every route (`src/shell/__root.tsx`), so this
// must resolve, not throw — `'unsupported'` renders nothing (same as
// `'idle'`) while being honest that no update mechanism exists here, not
// merely "nothing pending right now".

import type { UpdateStatus } from '../shared/protocol.js'

const UNSUPPORTED: UpdateStatus = { state: 'unsupported' }

export const update = {
  get: (): Promise<UpdateStatus> => Promise.resolve(UNSUPPORTED),
  check: (): Promise<UpdateStatus> => Promise.resolve(UNSUPPORTED),
  install: (): Promise<{ ok: boolean; error?: string }> =>
    Promise.resolve({ ok: false, error: 'Auto-update is not available in this build.' }),
  onStatus: (_cb: (status: UpdateStatus) => void): (() => void) => () => {},
}
