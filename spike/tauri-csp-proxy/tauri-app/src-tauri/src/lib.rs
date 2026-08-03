// Minimal spike for wayfinder ticket #22: identical to ../../tauri-hmr-check's
// app, except tauri.conf.json's window/devUrl point at the csp-proxy spike's
// listen port instead of Vite's real port directly. Nothing else to wire up
// here — the window itself is fully declared in tauri.conf.json.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
