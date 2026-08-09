// Pure schedule math for routines (Phase 7, tracking issue #27). Ported
// from `electron/main/routines/schedule.ts`. No I/O.

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};

const DAY_MS: i64 = 86_400_000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RoutineSchedule {
    Daily {
        time: String,
    },
    Interval {
        #[serde(rename = "everyMinutes")]
        every_minutes: f64,
    },
}

/// Validates `HH:MM` (24-hour), allowing a single-digit hour (`9:05`) —
/// deliberately looser than the IPC-layer validator in routines_commands.rs,
/// matching the same inconsistency the Electron original has between
/// `ipc-validate.ts`'s stricter two-digit-hour regex and this module's own.
fn is_valid_daily_time(time: &str) -> bool {
    let Some((h, m)) = time.split_once(':') else {
        return false;
    };
    if m.len() != 2 || !m.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    if h.is_empty() || h.len() > 2 || !h.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let hour: u32 = h.parse().unwrap_or(99);
    let min: u32 = m.parse().unwrap_or(99);
    hour <= 23 && min <= 59
}

pub fn validate_schedule(s: &RoutineSchedule) -> Result<(), String> {
    match s {
        RoutineSchedule::Interval { every_minutes } => {
            if !every_minutes.is_finite() || *every_minutes < 1.0 {
                return Err("Interval must be at least 1 minute.".to_string());
            }
        }
        RoutineSchedule::Daily { time } => {
            if !is_valid_daily_time(time) {
                return Err("Time must be HH:MM (24-hour).".to_string());
            }
        }
    }
    Ok(())
}

/// `after`/return value are epoch milliseconds, matching `Date.now()`'s unit.
pub fn compute_next_run(schedule: &RoutineSchedule, after: i64) -> i64 {
    match schedule {
        RoutineSchedule::Interval { every_minutes } => {
            let minutes = every_minutes.max(1.0);
            after + (minutes * 60_000.0) as i64
        }
        RoutineSchedule::Daily { time } => {
            let (h, m) = time.split_once(':').unwrap_or(("0", "0"));
            let hour: u32 = h.parse().unwrap_or(0);
            let minute: u32 = m.parse().unwrap_or(0);
            let base = Local.timestamp_millis_opt(after).unwrap();
            let candidate = base
                .date_naive()
                .and_hms_opt(hour, minute, 0)
                .and_then(|naive| Local.from_local_datetime(&naive).single())
                .map(|dt| dt.timestamp_millis())
                .unwrap_or(after);
            if candidate <= after {
                candidate + DAY_MS
            } else {
                candidate
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DueCheck {
    pub enabled: bool,
    pub next_run_at: Option<i64>,
}

pub fn is_due(routine: &DueCheck, now: i64) -> bool {
    routine.enabled && routine.next_run_at.is_some_and(|t| t <= now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_schedule_accepts_valid_interval_and_daily() {
        assert!(validate_schedule(&RoutineSchedule::Interval { every_minutes: 5.0 }).is_ok());
        assert!(validate_schedule(&RoutineSchedule::Daily {
            time: "09:05".to_string()
        })
        .is_ok());
        assert!(validate_schedule(&RoutineSchedule::Daily {
            time: "9:05".to_string()
        })
        .is_ok());
    }

    #[test]
    fn validate_schedule_rejects_invalid_input() {
        assert!(validate_schedule(&RoutineSchedule::Interval { every_minutes: 0.0 }).is_err());
        assert!(validate_schedule(&RoutineSchedule::Daily {
            time: "24:00".to_string()
        })
        .is_err());
        assert!(validate_schedule(&RoutineSchedule::Daily {
            time: "8h".to_string()
        })
        .is_err());
    }

    #[test]
    fn compute_next_run_interval_adds_exact_minute_span() {
        let next = compute_next_run(
            &RoutineSchedule::Interval {
                every_minutes: 10.0,
            },
            1_000_000,
        );
        assert_eq!(next, 1_000_000 + 10 * 60_000);
    }

    #[test]
    fn compute_next_run_daily_at_exact_current_minute_rolls_to_tomorrow() {
        let base = Local
            .with_ymd_and_hms(2026, 6, 2, 9, 5, 0)
            .unwrap()
            .timestamp_millis();
        let next = compute_next_run(
            &RoutineSchedule::Daily {
                time: "09:05".to_string(),
            },
            base,
        );
        assert_eq!(next, base + DAY_MS);
    }

    #[test]
    fn compute_next_run_daily_picks_next_local_time_later_same_day() {
        let base = Local
            .with_ymd_and_hms(2026, 6, 2, 8, 0, 0)
            .unwrap()
            .timestamp_millis();
        let expected = Local
            .with_ymd_and_hms(2026, 6, 2, 9, 5, 0)
            .unwrap()
            .timestamp_millis();
        let next = compute_next_run(
            &RoutineSchedule::Daily {
                time: "09:05".to_string(),
            },
            base,
        );
        assert_eq!(next, expected);
    }

    #[test]
    fn compute_next_run_daily_rolls_to_next_day_when_already_past() {
        let base = Local
            .with_ymd_and_hms(2026, 6, 2, 10, 0, 0)
            .unwrap()
            .timestamp_millis();
        let expected = Local
            .with_ymd_and_hms(2026, 6, 3, 9, 5, 0)
            .unwrap()
            .timestamp_millis();
        let next = compute_next_run(
            &RoutineSchedule::Daily {
                time: "09:05".to_string(),
            },
            base,
        );
        assert_eq!(next, expected);
    }

    #[test]
    fn is_due_returns_only_enabled_and_due() {
        assert!(is_due(
            &DueCheck {
                enabled: true,
                next_run_at: Some(100)
            },
            200
        ));
        assert!(!is_due(
            &DueCheck {
                enabled: false,
                next_run_at: Some(100)
            },
            200
        ));
        assert!(!is_due(
            &DueCheck {
                enabled: true,
                next_run_at: None
            },
            200
        ));
        assert!(!is_due(
            &DueCheck {
                enabled: true,
                next_run_at: Some(300)
            },
            200
        ));
    }
}
