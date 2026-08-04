// Confirms the boot watchdog once the renderer has actually mounted (Hearth#27
// Phase 1's `frontend_ready` command — see electron/preload-tauri/ready.ts and
// src-tauri/src/ready.rs for why this needs to be a post-mount signal, not a
// document-parsed one). Rendered as a sibling of the routed app inside
// ErrorBoundary: if anything in that tree throws during the initial render,
// React never commits this component's effect, so a crashing boot correctly
// never confirms. No-op under Electron, where there's no equivalent call.

import { useEffect } from 'react'
import { frontendReady } from '../../electron/preload-tauri/ready.js'

export function FrontendReadySignal(): null {
  useEffect(() => {
    if (window.__TAURI__) void frontendReady()
  }, [])
  return null
}
