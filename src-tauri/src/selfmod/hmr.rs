// HMR controller — applies an agent's edits to the running app at the cheapest
// reload tier the changed paths allow. Ported from electron/main/self-mod/hmr.ts.
//
// In dev, the Vite dev server already HMRs renderer edits on save — so for
// `Hmr`-tier changes the controller mostly observes. The work it owns is
// escalation: telling the window to reload, or asking main to restart, when an
// edit crosses a tier boundary.

use crate::selfmod::path_relevance::{classify_batch, ReloadKind};
use crate::selfmod::reload_driver::ReloadDriver;

pub struct HmrController<D: ReloadDriver> {
    driver: D,
    /// True when the renderer is served by a live Vite server (dev, and the
    /// packaged self-evolving build). In that mode Vite ALREADY reloads the
    /// page itself when a full-reload-tier file (route tree, index.html)
    /// changes on disk, so a second hard reload only doubles the black flash —
    /// skip it and let Vite's lighter reload stand. Only the static fallback
    /// (no Vite/HMR) needs the forced reload.
    vite_served: bool,
}

impl<D: ReloadDriver> HmrController<D> {
    pub fn new(driver: D, vite_served: bool) -> Self {
        Self {
            driver,
            vite_served,
        }
    }

    /// React to a committed batch of edits. Returns the tier that was applied
    /// so the caller can tell the user "reloaded" vs "restarting".
    pub fn apply(&self, changed_paths: &[String]) -> ReloadKind {
        let kind = classify_batch(changed_paths);
        match kind {
            ReloadKind::Hmr => {
                // Vite already hot-swapped on file write. Nothing to do.
            }
            ReloadKind::FullReload => {
                // The autonomous Vite reload is suppressed during the turn (B6), so
                // we trigger the reload here — behind the morph cover when Vite
                // serves the renderer (no black flash), or a plain reload for the
                // static fallback.
                if self.vite_served && self.driver.supports_covered_reload() {
                    self.driver.covered_reload();
                } else {
                    self.driver.reload_window();
                }
            }
            ReloadKind::ProcessRestart => {
                self.driver.restart_app();
            }
        }
        kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct FakeDriver {
        reload_calls: Cell<u32>,
        restart_calls: Cell<u32>,
        covered_calls: Cell<u32>,
        covered_supported: bool,
    }

    impl ReloadDriver for FakeDriver {
        fn reload_window(&self) {
            self.reload_calls.set(self.reload_calls.get() + 1);
        }
        fn restart_app(&self) {
            self.restart_calls.set(self.restart_calls.get() + 1);
        }
        fn supports_covered_reload(&self) -> bool {
            self.covered_supported
        }
        fn covered_reload(&self) {
            self.covered_calls.set(self.covered_calls.get() + 1);
        }
    }

    #[test]
    fn hmr_tier_is_a_noop() {
        let controller = HmrController::new(FakeDriver::default(), false);
        let kind = controller.apply(&["src/app/App.tsx".to_string()]);
        assert_eq!(kind, ReloadKind::Hmr);
        assert_eq!(controller.driver.reload_calls.get(), 0);
        assert_eq!(controller.driver.restart_calls.get(), 0);
        assert_eq!(controller.driver.covered_calls.get(), 0);
    }

    #[test]
    fn full_reload_static_fallback_calls_plain_reload() {
        let controller = HmrController::new(FakeDriver::default(), false);
        let kind = controller.apply(&["src/routes/chat.tsx".to_string()]);
        assert_eq!(kind, ReloadKind::FullReload);
        assert_eq!(controller.driver.reload_calls.get(), 1);
        assert_eq!(controller.driver.covered_calls.get(), 0);
    }

    #[test]
    fn full_reload_vite_served_without_covered_support_falls_back_to_plain_reload() {
        let controller = HmrController::new(FakeDriver::default(), true);
        let kind = controller.apply(&["index.html".to_string()]);
        assert_eq!(kind, ReloadKind::FullReload);
        assert_eq!(controller.driver.reload_calls.get(), 1);
        assert_eq!(controller.driver.covered_calls.get(), 0);
    }

    #[test]
    fn full_reload_vite_served_with_covered_support_uses_covered_reload() {
        let driver = FakeDriver {
            covered_supported: true,
            ..Default::default()
        };
        let controller = HmrController::new(driver, true);
        let kind = controller.apply(&["src/routeTree.gen.ts".to_string()]);
        assert_eq!(kind, ReloadKind::FullReload);
        assert_eq!(controller.driver.covered_calls.get(), 1);
        assert_eq!(controller.driver.reload_calls.get(), 0);
    }

    #[test]
    fn process_restart_tier_restarts_regardless_of_vite_served() {
        let controller = HmrController::new(FakeDriver::default(), true);
        let kind = controller.apply(&["electron/main/index.ts".to_string()]);
        assert_eq!(kind, ReloadKind::ProcessRestart);
        assert_eq!(controller.driver.restart_calls.get(), 1);
        assert_eq!(controller.driver.reload_calls.get(), 0);
        assert_eq!(controller.driver.covered_calls.get(), 0);
    }

    #[test]
    fn empty_batch_is_hmr_tier() {
        let controller = HmrController::new(FakeDriver::default(), true);
        let kind = controller.apply(&[]);
        assert_eq!(kind, ReloadKind::Hmr);
    }
}
