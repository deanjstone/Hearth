// The routines timer (Phase 7, tracking issue #27). Ported from
// `electron/main/routines/scheduler.ts`. `tick`/`run_now` are plain
// synchronous methods (fixture-testable with an injected clock, no real
// timer involved) — `spawn_ticker` is the thin wrapper that drives `tick` on
// a real interval, used only from `lib.rs`'s `setup()`.

use super::schedule::{is_due, DueCheck};
use super::store::{Routine, RoutineStore};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn real_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

pub struct RoutineScheduler {
    store: Arc<RoutineStore>,
    on_due: Box<dyn Fn(&Routine) + Send + Sync>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl RoutineScheduler {
    pub fn new(
        store: Arc<RoutineStore>,
        on_due: impl Fn(&Routine) + Send + Sync + 'static,
    ) -> Self {
        Self {
            store,
            on_due: Box::new(on_due),
            now: Box::new(real_now_ms),
        }
    }

    #[cfg(test)]
    fn with_clock(
        store: Arc<RoutineStore>,
        on_due: impl Fn(&Routine) + Send + Sync + 'static,
        now: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            store,
            on_due: Box::new(on_due),
            now: Box::new(now),
        }
    }

    /// A routine's schedule advances whether or not the renderer is ready
    /// (`mark_ran` runs before `on_due`), so a closed/busy app drops a fire
    /// rather than storming it. Any panic-shaped failure from `on_due` isn't
    /// caught here — Tauri's own event-emit call is infallible in practice
    /// (unlike Electron's `webContents.send`, which the original wraps in a
    /// try/catch for a torn-down renderer).
    pub fn tick(&self) {
        let now = (self.now)();
        let due: Vec<Routine> = self
            .store
            .list()
            .into_iter()
            .filter(|r| {
                is_due(
                    &DueCheck {
                        enabled: r.enabled,
                        next_run_at: r.next_run_at,
                    },
                    now,
                )
            })
            .collect();
        for routine in due {
            let _ = self.store.mark_ran(&routine.id, now);
            (self.on_due)(&routine);
        }
    }

    /// Fires immediately regardless of due-ness, stamping `last_run_at` to
    /// the current clock — same fire-once-advance-schedule semantics as a
    /// real tick, just manually triggered.
    pub fn run_now(&self, id: &str) {
        let Some(routine) = self.store.list().into_iter().find(|r| r.id == id) else {
            return;
        };
        let now = (self.now)();
        let _ = self.store.mark_ran(&routine.id, now);
        (self.on_due)(&routine);
    }
}

/// Drives `tick` on a real interval; the returned task should be aborted on
/// app shutdown (mirrors `clearInterval` in `stop()`).
pub fn spawn_ticker(
    scheduler: Arc<RoutineScheduler>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            scheduler.tick();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routines::schedule::RoutineSchedule;
    use crate::routines::store::CreateRoutineInput;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn make_store(dir: &TempDir) -> Arc<RoutineStore> {
        Arc::new(RoutineStore::new(dir.path().to_path_buf()))
    }

    #[test]
    fn tick_does_not_fire_a_not_due_routine() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        store
            .create(CreateRoutineInput {
                title: "t".to_string(),
                prompt: "p".to_string(),
                schedule: RoutineSchedule::Interval {
                    every_minutes: 60.0,
                },
                workspace_id: "hearth".to_string(),
                cwd: "/repo".to_string(),
            })
            .unwrap();
        let fired = Arc::new(Mutex::new(0));
        let fired_clone = fired.clone();
        let clock = Arc::new(Mutex::new(1_000_i64));
        let clock_clone = clock.clone();
        let scheduler = RoutineScheduler::with_clock(
            store,
            move |_| *fired_clone.lock().unwrap() += 1,
            move || *clock_clone.lock().unwrap(),
        );
        scheduler.tick();
        assert_eq!(*fired.lock().unwrap(), 0);
    }

    #[test]
    fn tick_fires_exactly_once_when_due_then_not_again_same_window() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let created = store
            .create(CreateRoutineInput {
                title: "t".to_string(),
                prompt: "p".to_string(),
                schedule: RoutineSchedule::Interval { every_minutes: 1.0 },
                workspace_id: "hearth".to_string(),
                cwd: "/repo".to_string(),
            })
            .unwrap();
        let fired = Arc::new(Mutex::new(0));
        let fired_clone = fired.clone();
        // Clock starts past the first next_run_at (created_at + 60_000ms).
        let clock = Arc::new(Mutex::new(created.next_run_at.unwrap() + 1));
        let clock_clone = clock.clone();
        let scheduler = RoutineScheduler::with_clock(
            store,
            move |_| *fired_clone.lock().unwrap() += 1,
            move || *clock_clone.lock().unwrap(),
        );
        scheduler.tick();
        assert_eq!(*fired.lock().unwrap(), 1);
        scheduler.tick();
        assert_eq!(
            *fired.lock().unwrap(),
            1,
            "must not re-fire in the same window"
        );
    }

    #[test]
    fn run_now_fires_immediately_regardless_of_due_ness() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let created = store
            .create(CreateRoutineInput {
                title: "t".to_string(),
                prompt: "p".to_string(),
                schedule: RoutineSchedule::Interval {
                    every_minutes: 60.0,
                },
                workspace_id: "hearth".to_string(),
                cwd: "/repo".to_string(),
            })
            .unwrap();
        let fired = Arc::new(Mutex::new(0));
        let fired_clone = fired.clone();
        let clock = Arc::new(Mutex::new(created.created_at + 5));
        let clock_clone = clock.clone();
        let scheduler = RoutineScheduler::with_clock(
            store,
            move |_| *fired_clone.lock().unwrap() += 1,
            move || *clock_clone.lock().unwrap(),
        );
        scheduler.run_now(&created.id);
        assert_eq!(*fired.lock().unwrap(), 1);
    }
}
