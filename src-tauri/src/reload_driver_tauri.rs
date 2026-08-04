// The real `ReloadDriver` (see selfmod/reload_driver.rs) backed by the app's
// actual window, wiring HmrController's escalation decisions to real Tauri
// APIs. Ported in spirit from the `hmr` construction in electron/main/index.ts
// (the object literal passed to `new HmrController({...}, ...)`), which has
// no standalone TS module of its own — this file is that wiring's Rust home.

use crate::selfmod::reload_driver::ReloadDriver;
use tauri::{Manager, WebviewWindow, Wry};

pub struct TauriReloadDriver {
    window: WebviewWindow<Wry>,
}

impl TauriReloadDriver {
    pub fn new(window: WebviewWindow<Wry>) -> Self {
        Self { window }
    }
}

impl ReloadDriver for TauriReloadDriver {
    fn reload_window(&self) {
        let _ = self.window.eval("location.reload()");
    }

    /// TS's restart_app only calls `app.relaunch()` when packaged — in dev,
    /// electron-vite owns the process lifecycle and restarting there would
    /// kill the dev session, so it falls back to a window reload. Same split
    /// here via `cfg!(debug_assertions)`, but with a caveat the TS version
    /// didn't have: relaunching restarts the *same compiled binary* — unlike
    /// Node, a Rust process-restart-tier self-mod edit needs a real
    /// recompile to actually take effect, which nothing in this port does
    /// yet. `AppHandle::restart` is still the correct call (it's what the
    /// boot watchdog's arm/revert cycle expects to run after), but closing
    /// that recompile gap is out of scope here.
    fn restart_app(&self) {
        if cfg!(debug_assertions) {
            let _ = self.window.eval("location.reload()");
        } else {
            self.window.app_handle().restart();
        }
    }
}
