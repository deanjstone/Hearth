// JSON-file persistence for routines (Phase 7, tracking issue #27). Ported
// from `electron/main/routines/store.ts`. One file, whole-array read/rewrite
// — no per-record file, matching `McpRegistry`'s own simpler-than-a-database
// approach for a small, human-scale list.

use super::schedule::{compute_next_run, validate_schedule, RoutineSchedule};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Routine {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub schedule: RoutineSchedule,
    pub workspace_id: String,
    pub cwd: String,
    pub enabled: bool,
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreateRoutineInput {
    pub title: String,
    pub prompt: String,
    pub schedule: RoutineSchedule,
    pub workspace_id: String,
    pub cwd: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct RoutinePatch {
    pub title: Option<String>,
    pub prompt: Option<String>,
    pub schedule: Option<RoutineSchedule>,
    pub workspace_id: Option<String>,
    pub cwd: Option<String>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn to_base36(mut n: u128) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

pub struct RoutineStore {
    path: PathBuf,
    counter: Mutex<u64>,
}

impl RoutineStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            path: base_dir.join("routines.json"),
            counter: Mutex::new(0),
        }
    }

    fn read(&self) -> Vec<Routine> {
        let Ok(text) = fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    fn write(&self, list: &[Routine]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
        fs::write(&self.path, json).map_err(|e| e.to_string())
    }

    pub fn list(&self) -> Vec<Routine> {
        let mut list = self.read();
        list.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        list
    }

    pub fn create(&self, input: CreateRoutineInput) -> Result<Routine, String> {
        validate_schedule(&input.schedule)?;
        let t = now_ms();
        let id = {
            let mut counter = self.counter.lock().unwrap();
            let id = format!("rt_{}_{}", to_base36(t as u128), *counter);
            *counter += 1;
            id
        };
        let title = if input.title.trim().is_empty() {
            "Routine".to_string()
        } else {
            input.title.trim().to_string()
        };
        let routine = Routine {
            id,
            title,
            prompt: input.prompt,
            next_run_at: Some(compute_next_run(&input.schedule, t)),
            schedule: input.schedule,
            workspace_id: input.workspace_id,
            cwd: input.cwd,
            enabled: true,
            created_at: t,
            last_run_at: None,
        };
        let mut list = self.read();
        list.insert(0, routine.clone());
        self.write(&list)?;
        Ok(routine)
    }

    fn patch(&self, id: &str, f: impl FnOnce(&mut Routine)) -> Result<Option<Routine>, String> {
        let mut list = self.read();
        let Some(idx) = list.iter().position(|r| r.id == id) else {
            return Ok(None);
        };
        f(&mut list[idx]);
        let updated = list[idx].clone();
        self.write(&list)?;
        Ok(Some(updated))
    }

    pub fn update(&self, id: &str, patch: RoutinePatch) -> Result<Option<Routine>, String> {
        if let Some(schedule) = &patch.schedule {
            validate_schedule(schedule)?;
        }
        self.patch(id, |r| {
            if let Some(title) = &patch.title {
                let trimmed = title.trim();
                if !trimmed.is_empty() {
                    r.title = trimmed.to_string();
                }
            }
            if let Some(prompt) = patch.prompt {
                r.prompt = prompt;
            }
            if let Some(schedule) = patch.schedule {
                r.schedule = schedule;
            }
            if let Some(workspace_id) = patch.workspace_id {
                r.workspace_id = workspace_id;
            }
            if let Some(cwd) = patch.cwd {
                r.cwd = cwd;
            }
            r.next_run_at = if r.enabled {
                Some(compute_next_run(&r.schedule, now_ms()))
            } else {
                None
            };
        })
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<Option<Routine>, String> {
        self.patch(id, |r| {
            r.enabled = enabled;
            r.next_run_at = if enabled {
                Some(compute_next_run(&r.schedule, now_ms()))
            } else {
                None
            };
        })
    }

    /// `ran_at` is the fire time — `next_run_at` advances from it, not from
    /// "now", matching the Electron original's distinction from
    /// `set_enabled`/`update`.
    pub fn mark_ran(&self, id: &str, ran_at: i64) -> Result<Option<Routine>, String> {
        self.patch(id, |r| {
            r.last_run_at = Some(ran_at);
            r.next_run_at = if r.enabled {
                Some(compute_next_run(&r.schedule, ran_at))
            } else {
                None
            };
        })
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut list = self.read();
        list.retain(|r| r.id != id);
        self.write(&list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn interval_input(minutes: f64) -> CreateRoutineInput {
        CreateRoutineInput {
            title: "Test".to_string(),
            prompt: "do the thing".to_string(),
            schedule: RoutineSchedule::Interval {
                every_minutes: minutes,
            },
            workspace_id: "hearth".to_string(),
            cwd: "/repo".to_string(),
        }
    }

    #[test]
    fn create_sets_next_run_at_and_persists() {
        let dir = TempDir::new().unwrap();
        let store = RoutineStore::new(dir.path().to_path_buf());
        let created = store.create(interval_input(5.0)).unwrap();
        assert!(created.next_run_at.is_some());
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, created.id);
    }

    #[test]
    fn create_rejects_an_invalid_schedule() {
        let dir = TempDir::new().unwrap();
        let store = RoutineStore::new(dir.path().to_path_buf());
        let mut input = interval_input(5.0);
        input.schedule = RoutineSchedule::Daily {
            time: "not-a-time".to_string(),
        };
        assert!(store.create(input).is_err());
    }

    #[test]
    fn disabling_clears_next_run_at_and_reenabling_recomputes_it() {
        let dir = TempDir::new().unwrap();
        let store = RoutineStore::new(dir.path().to_path_buf());
        let created = store.create(interval_input(5.0)).unwrap();
        let disabled = store.set_enabled(&created.id, false).unwrap().unwrap();
        assert_eq!(disabled.next_run_at, None);
        let reenabled = store.set_enabled(&created.id, true).unwrap().unwrap();
        assert!(reenabled.next_run_at.is_some());
    }

    #[test]
    fn mark_ran_stamps_last_run_at_and_advances_from_the_ran_time() {
        let dir = TempDir::new().unwrap();
        let store = RoutineStore::new(dir.path().to_path_buf());
        let created = store.create(interval_input(10.0)).unwrap();
        let ran_at = created.created_at + 1000;
        let updated = store.mark_ran(&created.id, ran_at).unwrap().unwrap();
        assert_eq!(updated.last_run_at, Some(ran_at));
        assert_eq!(updated.next_run_at, Some(ran_at + 10 * 60_000));
    }

    #[test]
    fn update_changing_schedule_recomputes_next_run_at() {
        let dir = TempDir::new().unwrap();
        let store = RoutineStore::new(dir.path().to_path_buf());
        let created = store.create(interval_input(5.0)).unwrap();
        let patch = RoutinePatch {
            schedule: Some(RoutineSchedule::Interval {
                every_minutes: 30.0,
            }),
            ..Default::default()
        };
        let updated = store.update(&created.id, patch).unwrap().unwrap();
        match updated.schedule {
            RoutineSchedule::Interval { every_minutes } => assert_eq!(every_minutes, 30.0),
            _ => panic!("expected interval schedule"),
        }
        assert!(updated.next_run_at.is_some());
    }

    #[test]
    fn remove_drops_it() {
        let dir = TempDir::new().unwrap();
        let store = RoutineStore::new(dir.path().to_path_buf());
        let created = store.create(interval_input(5.0)).unwrap();
        store.remove(&created.id).unwrap();
        assert!(store.list().is_empty());
    }
}
