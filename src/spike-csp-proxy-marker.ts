// Marker for wayfinder ticket #22's csp-proxy spike (spike/tauri-csp-proxy/).
// Unlike the HMR spike's SpikeMarker.ts, this one IS committed: it's dedicated
// solely to spike/tauri-csp-proxy/proxy-check.html (nothing else references
// it), so leaving it out would just 404 for anyone reproducing the spike from
// a fresh clone. Lives under src/ (not spike/) because vite.config.spike.ts
// ignores spike/** in its file watcher — this file must be watched for the
// HMR round-trip the spike is testing to mean anything.
//
// Reports to the /__spike/marker endpoint spike/vite.config.spike.ts exposes.
const MARKER_VALUE = 1

void fetch('/__spike/marker', {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ component: 'csp-proxy-check', value: MARKER_VALUE, host: 'tauri' }),
})

if (import.meta.hot) {
  import.meta.hot.accept(() => {})
}
