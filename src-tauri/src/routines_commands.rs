// The `window.hearth.routines` Tauri surface (Phase 7, tracking issue #27).
// Ported from the `routines:*` handlers in electron/main/ipc.ts, backed by
// `routines::store::RoutineStore`/`routines::scheduler::RoutineScheduler`.
//
// IPC-layer validation here (`assert_create_routine_input`) is deliberately
// STRICTER on daily-schedule time format (exactly two digits for the hour)
// than `schedule::validate_schedule`'s own looser check (single-digit hour
// allowed) — a real inconsistency the Electron original already has between
// `ipc-validate.ts` and `schedule.ts`, preserved rather than silently
// "fixed" by picking one.

use crate::routines::schedule::RoutineSchedule;
use crate::routines::scheduler::RoutineScheduler;
use crate::routines::store::{CreateRoutineInput, Routine, RoutinePatch, RoutineStore};
use std::sync::Arc;

pub struct RoutinesState {
    pub store: Arc<RoutineStore>,
    pub scheduler: Arc<RoutineScheduler>,
}

fn is_valid_daily_time_strict(time: &str) -> bool {
    let Some((h, m)) = time.split_once(':') else {
        return false;
    };
    if h.len() != 2 || m.len() != 2 {
        return false;
    }
    if !h.chars().all(|c| c.is_ascii_digit()) || !m.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let hour: u32 = h.parse().unwrap_or(99);
    let minute: u32 = m.parse().unwrap_or(99);
    hour <= 23 && minute <= 59
}

fn assert_schedule(schedule: &RoutineSchedule) -> Result<(), String> {
    match schedule {
        RoutineSchedule::Daily { time } => {
            if !is_valid_daily_time_strict(time) {
                return Err(r#"invalid schedule: daily schedule needs time "HH:MM""#.to_string());
            }
        }
        RoutineSchedule::Interval { every_minutes } => {
            if !every_minutes.is_finite() || *every_minutes <= 0.0 {
                return Err(
                    "invalid schedule: interval schedule needs a positive everyMinutes".to_string(),
                );
            }
        }
    }
    Ok(())
}

fn assert_create_routine_input(input: &CreateRoutineInput) -> Result<(), String> {
    if input.title.trim().is_empty() {
        return Err("invalid routine: title must be a non-empty string".to_string());
    }
    if input.prompt.trim().is_empty() {
        return Err("invalid routine: prompt must be a non-empty string".to_string());
    }
    assert_schedule(&input.schedule)
}

fn assert_routine_patch(patch: &RoutinePatch) -> Result<(), String> {
    if let Some(title) = &patch.title {
        if title.trim().is_empty() {
            return Err("invalid routine: title must be a non-empty string".to_string());
        }
    }
    if let Some(prompt) = &patch.prompt {
        if prompt.trim().is_empty() {
            return Err("invalid routine: prompt must be a non-empty string".to_string());
        }
    }
    if let Some(schedule) = &patch.schedule {
        assert_schedule(schedule)?;
    }
    Ok(())
}

#[tauri::command]
pub fn routines_list(state: tauri::State<RoutinesState>) -> Vec<Routine> {
    state.store.list()
}

#[tauri::command]
pub fn routines_create(
    state: tauri::State<RoutinesState>,
    input: CreateRoutineInput,
) -> Result<Routine, String> {
    assert_create_routine_input(&input)?;
    state.store.create(input)
}

#[tauri::command]
pub fn routines_update(
    state: tauri::State<RoutinesState>,
    id: String,
    patch: RoutinePatch,
) -> Result<Option<Routine>, String> {
    assert_routine_patch(&patch)?;
    state.store.update(&id, patch)
}

#[tauri::command]
pub fn routines_set_enabled(
    state: tauri::State<RoutinesState>,
    id: String,
    enabled: bool,
) -> Result<Option<Routine>, String> {
    state.store.set_enabled(&id, enabled)
}

#[tauri::command]
pub fn routines_delete(state: tauri::State<RoutinesState>, id: String) -> Result<(), String> {
    state.store.remove(&id)
}

#[tauri::command]
pub fn routines_run_now(state: tauri::State<RoutinesState>, id: String) {
    state.scheduler.run_now(&id);
}
