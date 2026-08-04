// JS side of the "frontend-ready" event (Hearth#27 Phase 1's IPC scope),
// backed by src-tauri/src/ready.rs's `frontend_ready` command.
//
// Call this once the renderer has actually mounted (not just once the HTML
// document parsed) — see ready.rs's doc comment for why that distinction
// matters to the boot watchdog. There's no Electron-side equivalent to
// mirror here: the original relies on `did-finish-load`, a native
// webContents event with no renderer-side call at all.

interface TauriGlobal {
  core: {
    invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>
  }
}

export function frontendReady(): Promise<void> {
  const g = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__
  if (!g) {
    throw new Error('preload-tauri/ready: window.__TAURI__ is not present (is withGlobalTauri enabled?)')
  }
  return g.core.invoke('frontend_ready') as Promise<void>
}
