// Shared window.__TAURI__ access for preload-tauri shims. Extracted once
// Chunk 5 (spec deanjstone/Hearth#48) needed the same TauriGlobal interface +
// guard in three new files at once (agent.ts, permission.ts, auth.ts) — a
// third+ copy of what self-mod.ts/ready.ts/view.ts/agent-runtime.ts each
// already carry crossed the point where extracting was worth it. Those four
// existing files keep their own copies unchanged (not retrofitted here, to
// avoid unrelated churn); new shims should import from here instead of
// re-declaring their own.
//
// Deliberately still window.__TAURI__ (withGlobalTauri), not the
// @tauri-apps/api npm package — self-mod.ts's own doc comment frames that
// package as deferred "until a later chunk's preload surface needs more of
// it"; Chunk 5 needs event.listen for the first time, and it's simple enough
// to type by hand here too, so that threshold still hasn't been crossed.

export type UnlistenFn = () => void

interface TauriGlobal {
  core: {
    invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>
  }
  event: {
    listen: <T>(event: string, handler: (e: { payload: T }) => void) => Promise<UnlistenFn>
  }
}

export function tauri(): TauriGlobal {
  const g = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__
  if (!g) {
    throw new Error('preload-tauri: window.__TAURI__ is not present (is withGlobalTauri enabled?)')
  }
  return g
}

/**
 * Subscribe to a Tauri event with the same synchronous-unsubscribe shape
 * Electron's preload `onX` methods have (`ipcRenderer.on` is synchronous;
 * Tauri's `event.listen` is async, returning `Promise<UnlistenFn>`). If the
 * caller unsubscribes before that promise resolves, `cancelled` makes the
 * real unlisten fire the instant it does arrive instead of leaking a
 * registered listener forever.
 */
export function onEvent<T>(event: string, cb: (payload: T) => void): UnlistenFn {
  let unlisten: UnlistenFn | undefined
  let cancelled = false
  void tauri()
    .event.listen<T>(event, (e) => cb(e.payload))
    .then((u) => {
      if (cancelled) u()
      else unlisten = u
    })
  return () => {
    cancelled = true
    unlisten?.()
  }
}
