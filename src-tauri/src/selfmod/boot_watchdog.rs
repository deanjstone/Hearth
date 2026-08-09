// Boot watchdog (W6). The renderer crash surface can't recover a broken
// main-process self-edit — main is down. This guard catches that catastrophic
// case: every self-mod-triggered restart is "armed" with a marker recording the
// commit. Main clears the marker only once it reaches ready. If a boot finds the
// marker STILL present (the previous restart never reached ready → it bricked
// startup), it auto-reverts that commit and relaunches. A bounded attempt count
// stops a poisoned revert from looping into its own crash cycle.
//
// Ported from electron/main/self-mod/boot-watchdog.ts. The git revert +
// relaunch themselves live in the app bootstrap, driven by `inspect_boot()`'s
// decision. See docs/completed-plans/SELF-MOD-HARDENING-PLAN.md (W6).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Marker {
    commit: String,
    /// ISO timestamp the restart was armed (passed in — no clock reads in tests).
    #[serde(rename = "armedAt")]
    armed_at: String,
    /// How many recovery attempts (reverts) have already been made for this marker.
    attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootDecision {
    None,
    Revert { commit: String, attempt: u32 },
    SafeMode { commit: String },
}

pub struct BootWatchdog {
    marker_path: PathBuf,
    max_attempts: u32,
}

impl BootWatchdog {
    pub fn new(marker_path: impl Into<PathBuf>) -> Self {
        Self::with_max_attempts(marker_path, 2)
    }

    pub fn with_max_attempts(marker_path: impl Into<PathBuf>, max_attempts: u32) -> Self {
        Self {
            marker_path: marker_path.into(),
            max_attempts,
        }
    }

    fn read(&self) -> Option<Marker> {
        let contents = fs::read_to_string(&self.marker_path).ok()?;
        // A corrupt marker is treated as absent rather than wedging boot.
        serde_json::from_str(&contents).ok()
    }

    fn write(&self, marker: &Marker) {
        if let Some(parent) = self.marker_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(marker) {
            let _ = fs::write(&self.marker_path, json);
        }
    }

    /// Arm a self-mod restart. Call right before relaunch. `now` is injected.
    pub fn arm(&self, commit: &str, now: &str) {
        self.write(&Marker {
            commit: commit.to_string(),
            armed_at: now.to_string(),
            attempts: 0,
        });
    }

    /// Boot reached a healthy ready state — clear the marker.
    pub fn confirm_ready(&self) {
        let _ = fs::remove_file(&self.marker_path);
    }

    /// Inspect the marker at the very start of boot. If a prior self-mod restart
    /// never confirmed ready, decide how to recover:
    ///   - within the attempt budget → revert that commit (and record the attempt),
    ///   - budget exhausted → safe-mode (don't keep reverting into a crash loop),
    ///   - no marker → nothing to do.
    pub fn inspect_boot(&self) -> BootDecision {
        let Some(m) = self.read() else {
            return BootDecision::None;
        };
        if m.attempts >= self.max_attempts {
            return BootDecision::SafeMode { commit: m.commit };
        }
        let attempt = m.attempts + 1;
        self.write(&Marker {
            attempts: attempt,
            ..m.clone()
        });
        BootDecision::Revert {
            commit: m.commit,
            attempt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};

    fn temp_marker() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir
            .path()
            .join("state")
            .join("pending-self-mod-restart.json");
        (dir, marker)
    }

    #[test]
    fn clean_boot_no_action() {
        let (_dir, marker) = temp_marker();
        let wd = BootWatchdog::new(&marker);
        assert_eq!(wd.inspect_boot(), BootDecision::None);
    }

    #[test]
    fn healthy_restart_arm_confirm_then_clean() {
        let (_dir, marker) = temp_marker();
        let wd = BootWatchdog::new(&marker);
        wd.arm("abc123", "1970-01-01T00:00:00.000Z");
        assert!(marker.exists());
        wd.confirm_ready();
        assert!(!marker.exists());
        assert_eq!(wd.inspect_boot(), BootDecision::None);
    }

    #[test]
    fn bricked_boot_reverts_with_attempt_count() {
        let (_dir, marker) = temp_marker();
        let wd = BootWatchdog::new(&marker);
        wd.arm("deadbeef", "1970-01-01T00:00:00.000Z");
        assert_eq!(
            wd.inspect_boot(),
            BootDecision::Revert {
                commit: "deadbeef".to_string(),
                attempt: 1
            }
        );
    }

    #[test]
    fn retry_cap_falls_back_to_safe_mode() {
        let (_dir, marker) = temp_marker();
        let wd = BootWatchdog::with_max_attempts(&marker, 2);
        wd.arm("deadbeef", "1970-01-01T00:00:00.000Z");
        assert!(matches!(
            wd.inspect_boot(),
            BootDecision::Revert { attempt: 1, .. }
        ));
        assert!(matches!(
            wd.inspect_boot(),
            BootDecision::Revert { attempt: 2, .. }
        ));
        assert_eq!(
            wd.inspect_boot(),
            BootDecision::SafeMode {
                commit: "deadbeef".to_string()
            }
        );
    }

    #[test]
    fn corrupt_marker_does_not_wedge_boot() {
        let (_dir, marker) = temp_marker();
        let wd = BootWatchdog::new(&marker);
        wd.arm("x", "1970-01-01T00:00:00.000Z");
        create_dir_all(marker.parent().unwrap()).unwrap();
        write(&marker, "{not json").unwrap();
        assert_eq!(wd.inspect_boot(), BootDecision::None);
    }
}
