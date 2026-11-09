//! The Quest Board scheduler: when jobs fire, and the loop that fires them.

use std::str::FromStr;
use std::time::Duration;

use boba_core::api::SchedulePreview;
use boba_core::automation::Schedule;
use chrono::{DateTime, Utc};
use croner::Cron;

use crate::state::SharedState;

pub const TICK: Duration = Duration::from_secs(15);
pub const MIN_INTERVAL_MINUTES: u32 = 5;

fn tz(name: &str) -> Result<chrono_tz::Tz, String> {
    chrono_tz::Tz::from_str(name.trim())
        .map_err(|_| format!("unknown time zone \"{name}\" (use an IANA name like Europe/Paris)"))
}

fn cron(expr: &str) -> Result<Cron, String> {
    let fields = expr.split_whitespace().count();
    if fields != 5 {
        return Err(format!(
            "a cron expression has 5 fields (minute hour day month weekday), got {fields}"
        ));
    }
    Cron::from_str(expr.trim()).map_err(|e| format!("invalid cron expression: {e}"))
}

pub fn validate(s: &Schedule) -> Result<(), String> {
    match s {
        Schedule::Manual => Ok(()),
        Schedule::Interval { minutes } if *minutes < MIN_INTERVAL_MINUTES => Err(format!(
            "the shortest interval is {MIN_INTERVAL_MINUTES} minutes"
        )),
        Schedule::Interval { minutes } if *minutes > 60 * 24 * 366 => {
            Err("that interval is longer than a year".into())
        }
        Schedule::Interval { .. } => Ok(()),
        Schedule::Cron { expr, timezone } => {
            tz(timezone)?;
            cron(expr)?;
            Ok(())
        }
    }
}

/// The first fire time strictly after `after` (None for manual jobs).
pub fn next_after(s: &Schedule, after: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, String> {
    match s {
        Schedule::Manual => Ok(None),
        Schedule::Interval { minutes } => Ok(Some(
            after + chrono::Duration::minutes((*minutes).max(MIN_INTERVAL_MINUTES) as i64),
        )),
        Schedule::Cron { expr, timezone } => {
            let tz = tz(timezone)?;
            let c = cron(expr)?;
            let local = after.with_timezone(&tz);
            c.find_next_occurrence(&local, false)
                .map(|d| Some(d.with_timezone(&Utc)))
                .map_err(|e| format!("this schedule never fires: {e}"))
        }
    }
}

/// "Every Monday at 09:00 (Europe/Paris)".
pub fn describe(s: &Schedule) -> String {
    match s {
        Schedule::Manual => "Only when you press Run now".into(),
        Schedule::Interval { minutes } => match *minutes {
            m if m % (60 * 24) == 0 && m / (60 * 24) == 1 => "Every day".into(),
            m if m % (60 * 24) == 0 => format!("Every {} days", m / (60 * 24)),
            60 => "Every hour".into(),
            m if m % 60 == 0 => format!("Every {} hours", m / 60),
            m => format!("Every {m} minutes"),
        },
        Schedule::Cron { expr, timezone } => match cron(expr) {
            Ok(c) => {
                let mut d = c.describe();
                if let Some(first) = d.get(..1) {
                    d = first.to_uppercase() + &d[1..];
                }
                format!("{} ({})", d.trim_end_matches('.'), timezone.trim())
            }
            Err(_) => expr.clone(),
        },
    }
}

pub fn preview(s: &Schedule, now: DateTime<Utc>) -> SchedulePreview {
    if let Err(e) = validate(s) {
        return SchedulePreview {
            valid: false,
            error: Some(e),
            next: Vec::new(),
            description: String::new(),
        };
    }
    let mut next = Vec::new();
    let mut at = now;
    while next.len() < 5 {
        match next_after(s, at) {
            Ok(Some(t)) => {
                next.push(t);
                at = t;
            }
            Ok(None) => break,
            Err(e) => {
                return SchedulePreview {
                    valid: false,
                    error: Some(e),
                    next: Vec::new(),
                    description: String::new(),
                }
            }
        }
    }
    SchedulePreview {
        valid: true,
        error: None,
        next,
        description: describe(s),
    }
}

/// Fire due jobs every [`TICK`]. Jobs whose time passed while the server was down fire
/// once on the first tick, then resume their normal rhythm.
pub fn spawn(state: SharedState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = crate::jobs::run_due(&state, Utc::now()).await {
                tracing::warn!(error = ?e, "scheduler tick");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cron_in_a_time_zone() {
        let s = Schedule::Cron {
            expr: "0 9 * * 1".into(),
            timezone: "Europe/Paris".into(),
        };
        // Sunday 2026-11-08 12:00 UTC -> Monday 09:00 Paris = 08:00 UTC (winter time).
        let after = Utc.with_ymd_and_hms(2026, 11, 8, 12, 0, 0).unwrap();
        let next = next_after(&s, after).unwrap().unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 11, 9, 8, 0, 0).unwrap());
        let p = preview(&s, after);
        assert!(p.valid);
        assert_eq!(p.next.len(), 5);
        assert_eq!(
            p.next[1],
            Utc.with_ymd_and_hms(2026, 11, 16, 8, 0, 0).unwrap()
        );
        assert!(p.description.contains("Europe/Paris"), "{}", p.description);
    }

    #[test]
    fn invalid_schedules_are_explained() {
        let bad = [
            Schedule::Interval { minutes: 1 },
            Schedule::Cron {
                expr: "0 9 * *".into(),
                timezone: "UTC".into(),
            },
            Schedule::Cron {
                expr: "0 9 * * 1".into(),
                timezone: "Mars/Olympus".into(),
            },
            Schedule::Cron {
                expr: "99 9 * * 1".into(),
                timezone: "UTC".into(),
            },
        ];
        for s in bad {
            let p = preview(&s, Utc::now());
            assert!(!p.valid && p.error.is_some(), "{s:?}");
        }
        assert!(preview(&Schedule::Manual, Utc::now()).next.is_empty());
        assert_eq!(
            describe(&Schedule::Interval { minutes: 120 }),
            "Every 2 hours"
        );
    }
}
