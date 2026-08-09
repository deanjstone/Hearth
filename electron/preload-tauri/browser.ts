// window.hearth.browser's Tauri stub (Phase 7, tracking issue #27). The
// embedded in-app browser is an Electron `WebContentsView` overlaid on the
// renderer via native bounds — Tauri has no equivalent primitive, and
// porting one is out of scope for this MVP (see the Phase 7 IPC-scope
// triage: Browser tab was deliberately left unported). Every method here is
// a safe no-op rather than a throw, so call sites (`BrowserTab.tsx`,
// `ConnectorsSection.tsx`'s one `navigate` call) don't crash; `BrowserTab.tsx`
// itself checks `window.__TAURI__` and renders an "unavailable" notice
// instead of mounting these handlers for real.

import type { BrowserState } from '../main/browser/browser-view.js'

const UNAVAILABLE_STATE: BrowserState = {
  url: '',
  title: 'Browser tab is not available in this build',
  loading: false,
  canGoBack: false,
  canGoForward: false,
}

export const browser = {
  open: (_workspaceId: string | undefined, _fallback: string): void => {},
  navigate: (_url: string, _workspaceId?: string): void => {},
  back: (): void => {},
  forward: (): void => {},
  reload: (): void => {},
  setBounds: (_rect: { x: number; y: number; width: number; height: number }): void => {},
  hide: (): void => {},
  onState: (cb: (state: BrowserState) => void): (() => void) => {
    cb(UNAVAILABLE_STATE)
    return () => {}
  },
}
