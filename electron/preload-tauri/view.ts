// JS-side compatibility shim for `window.hearth.view.onNavigate`, backed by a
// Tauri event (src-tauri/src/bridge.rs's `capture_snapshot` emits
// "view:navigate" to the offscreen snapshot window) instead of Electron's
// `ipcRenderer.on(CH.viewNavigate, ...)`. Exposes the same shape
// electron/preload/index.ts's `view` object does — see src/main.tsx's call
// site, which routes the (memory-history, non-URL-driven) app before an
// agent snapshot capture.
//
// Uses the `window.__TAURI__` global (withGlobalTauri: true), same as
// self-mod.ts/ready.ts — no `@tauri-apps/api` npm dependency needed.

interface TauriEventPayload<T> {
  payload: T
}

interface TauriGlobal {
  event: {
    listen: <T>(event: string, handler: (event: TauriEventPayload<T>) => void) => Promise<() => void>
  }
}

function tauri(): TauriGlobal {
  const g = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__
  if (!g) {
    throw new Error('preload-tauri/view: window.__TAURI__ is not present (is withGlobalTauri enabled?)')
  }
  return g
}

export const view = {
  onNavigate: (cb: (payload: { path: string }) => void): (() => void) => {
    let unlisten: (() => void) | null = null
    let cancelled = false
    tauri()
      .event.listen<{ path: string }>('view:navigate', (event) => cb(event.payload))
      .then((fn) => {
        if (cancelled) fn()
        else unlisten = fn
      })
    return () => {
      cancelled = true
      unlisten?.()
    }
  },
}
