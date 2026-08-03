// Minimal spike: prove nothing beyond "does the WebviewWindow this config points
// at Hearth's real Vite dev server survive the self-mod HMR trick". The window
// itself is fully declared in tauri.conf.json (app.windows[0].url = the spike
// Vite server), so there's nothing else to wire up here.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
