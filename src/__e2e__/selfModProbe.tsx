// Dedicated HMR self-mod swap probe for the permanent tauri-driver +
// WebDriverIO CI suite (e2e-tests/specs/hmr-self-mod.spec.js). Deliberately a
// throwaway component with no product purpose — mirrors
// spike/tauri-hmr-check/'s marker-module approach, but lives in the real
// src/ tree so the test exercises the real app/build rather than a synthetic
// shell.
//
// Mounted directly (and hidden) from main.tsx rather than routed: the app's
// router uses createMemoryHistory (see main.tsx's comment — "this is a
// desktop shell, not a browser"), so a WebDriver URL navigation can't reach
// a routed page; real navigation is IPC-driven (window.hearth.view, Phase 2,
// not built yet). Mounting unconditionally sidesteps that entirely and
// works regardless of which route is active.
//
// The test edits PROBE_VALUE on disk, POSTs pin/apply to the Vite overlay's
// /__hearth/self-mod endpoint, and asserts the live DOM reflects the new
// value via the stable data-testid below — kept separate from any real UI
// selector so unrelated UI changes can't break this test.
//
// Deliberately a single component export, nothing else — React Fast
// Refresh falls back to a full page reload (not a targeted swap) for a
// module that mixes a component export with a plain value export, which
// would defeat the "apply doesn't trigger a full reload" assertion this
// probe exists for. PROBE_VALUE stays a local const, not a separate export.

export function SelfModProbe() {
  const PROBE_VALUE = 'probe-v1'
  return (
    <div
      data-testid="self-mod-probe"
      style={{ position: 'fixed', left: -9999, top: -9999, pointerEvents: 'none' }}
    >
      {PROBE_VALUE}
    </div>
  )
}
