// W2 (Phase 6, tracking issue #27): deny every powerful device-permission
// request (camera, mic, geolocation, notifications, …) on a webview, via
// WebKitGTK's native `permission-request` signal. Applied to every
// `WebviewWindow` this app ever creates — today, the main shell window
// (lib.rs's `setup()`) and bridge.rs's lazily-created offscreen
// route-capture window (`ensure_offscreen`), since a route-captured
// `/tools` view renders `MicroAppFrame` there too and needs the same
// coverage the main window gets.
//
// Extracted into a shared function rather than duplicated per call site
// (unlike most of this phase's other small copies, e.g. broker.rs's
// `header_value`) specifically because this is a security control, not a
// convenience — #20's own framing: "any gap here is a regression". A third
// window added later that forgets to call this would silently reopen the
// gap; one shared call site is easier to audit for completeness than N
// duplicated ones.
//
// Micro-apps render as `<iframe>`s inside whichever webview embeds them
// (src/shell/MicroAppFrame.tsx), so denying at the webview level covers
// every micro-app frame inside it too — no per-app wiring needed, matching
// Electron's session-wide `session.setPermissionRequestHandler` posture.

#[cfg(target_os = "linux")]
pub fn deny_all_permissions(window: &tauri::WebviewWindow<tauri::Wry>) -> tauri::Result<()> {
    use webkit2gtk::{PermissionRequestExt, WebViewExt};
    window.with_webview(|webview| {
        webview
            .inner()
            .connect_permission_request(|_webview, request| {
                request.deny();
                true
            });
    })
}

#[cfg(not(target_os = "linux"))]
pub fn deny_all_permissions(_window: &tauri::WebviewWindow<tauri::Wry>) -> tauri::Result<()> {
    eprintln!("[hearth] permission deny-all is only implemented for WebKitGTK (Linux)");
    Ok(())
}
