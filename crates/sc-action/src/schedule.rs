//! When a periodic trigger is due (design §10.2, decision 6).
//!
//! Four kinds of schedule, no cron expression: `often` (every five minutes),
//! `hourly`, `daily` and `weekly`. That is deliberate — a cron string is a second
//! language to learn, to validate and to render a form for, and the four kinds
//! cover what a trigger is actually used for. A trigger that genuinely needs
//! "every third Tuesday" is a trigger whose action should be deciding that.
//!
//! **Everything is UTC** (decision 6). A server-side schedule has no user to have
//! a timezone, and a local one would mean an hour that happens twice a year and an
//! hour that does not happen at all — so `daily` at `03:00` is `03:00Z`, always,
//! and the arithmetic below needs no timezone database to be right.
//!
//! The timing lives in the trigger's [`attributes`](Trigger::attributes) (§9's
//! sparse rule: three values that only a periodic trigger has), and is read
//! through [`Schedule::of`] — which is also the validator, so save-time checking
//! and fire-time computation cannot disagree about what a stored trigger means.

use chrono::{DateTime, Duration, Timelike, Utc};
use sc_error::{Error, Result};

use crate::event::EventKind;
use crate::trigger::Trigger;

/// The minute past the hour an `hourly`, `daily` or `weekly` trigger fires at.
pub const ATTR_MINUTE: &str = "minute";
/// The hour of the day (UTC) a `daily` or `weekly` trigger fires at.
pub const ATTR_HOUR: &str = "hour";
/// The day of the week a `weekly` trigger fires on: **0 = Monday … 6 = Sunday**
/// (chrono's `num_days_from_monday`). The admin UI shows day names, so the
/// convention only has to be consistent, not memorable.
pub const ATTR_DAY_OF_WEEK: &str = "day_of_week";

/// Every timing attribute, for the "this kind does not take that" check.
const TIMING_ATTRS: [&str; 3] = [ATTR_MINUTE, ATTR_HOUR, ATTR_DAY_OF_WEEK];

/// How often an `often` trigger fires. Fixed rather than configurable: "often"
/// is the kind you pick when you do not want to think about the timing.
pub const OFTEN_MINUTES: i64 = 5;

/// When a periodic trigger fires, resolved from its kind and its timing
/// attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// Every [`OFTEN_MINUTES`] minutes, from whenever the last run was.
    Often,
    /// Once an hour, at `minute` past.
    Hourly {
        /// Minute past the hour, 0–59.
        minute: u32,
    },
    /// Once a day, at `hour:minute` UTC.
    Daily {
        /// Hour of the day, 0–23.
        hour: u32,
        /// Minute past the hour, 0–59.
        minute: u32,
    },
    /// Once a week, on `day_of_week` at `hour:minute` UTC.
    Weekly {
        /// 0 = Monday … 6 = Sunday.
        day_of_week: u32,
        /// Hour of the day, 0–23.
        hour: u32,
        /// Minute past the hour, 0–59.
        minute: u32,
    },
}

impl Schedule {
    /// The schedule `trigger` fires on, or `None` when it is not a periodic
    /// trigger at all.
    ///
    /// This is the **validator too**, and it is called both on save and on load:
    /// a value out of range is refused by name, and a timing attribute on a kind
    /// that has no use for it (a `minute` on a `daily`… or on a `login`) is
    /// refused rather than silently ignored — the same rule a channel on a
    /// channel-less event gets, for the same reason. A setting that is quietly
    /// dropped is one the admin believes is in effect.
    ///
    /// An unset value defaults to 0, so a `daily` trigger nobody has configured a
    /// time for fires at midnight UTC rather than refusing to be saved. That is
    /// the forgiving direction: the trigger runs once a day, which is what its
    /// kind said.
    pub fn of(trigger: &Trigger) -> Result<Option<Schedule>> {
        let used: &[&str] = match trigger.when {
            EventKind::Often => &[],
            EventKind::Hourly => &[ATTR_MINUTE],
            EventKind::Daily => &[ATTR_MINUTE, ATTR_HOUR],
            EventKind::Weekly => &[ATTR_MINUTE, ATTR_HOUR, ATTR_DAY_OF_WEEK],
            // Not periodic: it has no schedule, and it must not carry timing
            // either.
            _ => {
                if let Some(stray) = TIMING_ATTRS
                    .iter()
                    .find(|a| trigger.attributes.get(**a).is_some_and(|v| !v.is_null()))
                {
                    return Err(Error::invalid(format!(
                        "an `{}` event does not fire on a schedule, so `{stray}` has \
                         no meaning on it",
                        trigger.when
                    )));
                }
                return Ok(None);
            }
        };
        if let Some(stray) = TIMING_ATTRS
            .iter()
            .filter(|a| !used.contains(a))
            .find(|a| trigger.attributes.get(**a).is_some_and(|v| !v.is_null()))
        {
            return Err(Error::invalid(format!(
                "an `{}` trigger has no `{stray}` to set",
                trigger.when
            )));
        }

        let minute = number(trigger, ATTR_MINUTE, 59)?;
        let hour = number(trigger, ATTR_HOUR, 23)?;
        let day_of_week = number(trigger, ATTR_DAY_OF_WEEK, 6)?;
        Ok(Some(match trigger.when {
            EventKind::Often => Schedule::Often,
            EventKind::Hourly => Schedule::Hourly { minute },
            EventKind::Daily => Schedule::Daily { hour, minute },
            _ => Schedule::Weekly {
                day_of_week,
                hour,
                minute,
            },
        }))
    }

    /// The first time this schedule fires **strictly after** `after`.
    ///
    /// `after` is the previous run (or, for a trigger nobody has run yet, when it
    /// was first seen — never the epoch, which would make every fresh trigger
    /// instantly overdue).
    pub fn next_due(&self, after: DateTime<Utc>) -> DateTime<Utc> {
        match *self {
            // Relative to the last run rather than to a wall-clock grid: "every
            // five minutes" is a spacing, not an appointment.
            Schedule::Often => after + Duration::minutes(OFTEN_MINUTES),
            Schedule::Hourly { minute } => {
                let candidate = at_minute(after, minute);
                if candidate > after {
                    candidate
                } else {
                    candidate + Duration::hours(1)
                }
            }
            Schedule::Daily { hour, minute } => {
                let candidate = at_time(after, hour, minute);
                if candidate > after {
                    candidate
                } else {
                    candidate + Duration::days(1)
                }
            }
            Schedule::Weekly {
                day_of_week,
                hour,
                minute,
            } => {
                // At most eight steps, and no timezone arithmetic to get wrong
                // because UTC days are all 24 hours long.
                let mut candidate = at_time(after, hour, minute);
                while candidate <= after || weekday_number(candidate) != day_of_week {
                    candidate += Duration::days(1);
                }
                candidate
            }
        }
    }

    /// Whether this schedule is due at `now`, given when it last ran.
    pub fn is_due(&self, now: DateTime<Utc>, last_run: DateTime<Utc>) -> bool {
        self.next_due(last_run) <= now
    }
}

impl std::fmt::Display for Schedule {
    /// A one-line description, for the admin list and for error messages.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Schedule::Often => write!(f, "every {OFTEN_MINUTES} minutes"),
            Schedule::Hourly { minute } => write!(f, "hourly at :{minute:02}"),
            Schedule::Daily { hour, minute } => write!(f, "daily at {hour:02}:{minute:02} UTC"),
            Schedule::Weekly {
                day_of_week,
                hour,
                minute,
            } => write!(
                f,
                "weekly on {} at {hour:02}:{minute:02} UTC",
                day_name(day_of_week)
            ),
        }
    }
}

/// The English name of a day number (0 = Monday), or the number itself if it is
/// somehow out of range — this is a label, not a place to fail.
pub fn day_name(day_of_week: u32) -> String {
    [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ]
    .get(day_of_week as usize)
    .map_or_else(|| day_of_week.to_string(), |name| (*name).to_owned())
}

/// A timing attribute as a number in `0..=max`, defaulting to 0 when unset.
fn number(trigger: &Trigger, key: &str, max: u64) -> Result<u32> {
    let Some(value) = trigger.attributes.get(key).filter(|v| !v.is_null()) else {
        return Ok(0);
    };
    let out_of_range = || {
        Error::invalid(format!(
            "`{key}` must be a whole number between 0 and {max}, got {value}"
        ))
    };
    let n = value.as_u64().ok_or_else(out_of_range)?;
    if n > max {
        return Err(out_of_range());
    }
    u32::try_from(n).map_err(|_| out_of_range())
}

/// `when`'s date and hour at `minute` past, seconds cleared.
fn at_minute(when: DateTime<Utc>, minute: u32) -> DateTime<Utc> {
    at_time(when, when.hour(), minute)
}

/// `when`'s date at `hour:minute:00`.
///
/// Total by construction: an out-of-range hour or minute falls back to midnight
/// rather than panicking. It cannot happen — [`Schedule::of`] is the only way to
/// build a schedule and it refuses those values — but a scheduler is the last
/// place to leave a panic path, and "fires at midnight" is a visible wrong answer
/// where a panicking clock task would be an invisible one.
///
/// Clearing the seconds is not cosmetic: without it a run at 10:00:37 would set
/// the next daily due time to 03:30:37, and the drift would accumulate.
fn at_time(when: DateTime<Utc>, hour: u32, minute: u32) -> DateTime<Utc> {
    let time = chrono::NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or(chrono::NaiveTime::MIN);
    when.date_naive().and_time(time).and_utc()
}

/// The day number this instant falls on, 0 = Monday.
fn weekday_number(when: DateTime<Utc>) -> u32 {
    chrono::Datelike::weekday(&when).num_days_from_monday()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value as Json, json};

    /// A trigger of `kind` carrying `timing` as its attributes.
    fn trigger(kind: EventKind, timing: &[(&str, Json)]) -> Trigger {
        let mut t = Trigger::new("t", kind, "fetch");
        for (key, value) in timing {
            t.attributes.insert((*key).to_owned(), value.clone());
        }
        t
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("a valid timestamp")
            .with_timezone(&Utc)
    }

    #[test]
    fn each_periodic_kind_reads_its_own_timing_and_nothing_else() {
        assert_eq!(
            Schedule::of(&trigger(EventKind::Often, &[])).unwrap(),
            Some(Schedule::Often)
        );
        assert_eq!(
            Schedule::of(&trigger(EventKind::Hourly, &[(ATTR_MINUTE, json!(20))])).unwrap(),
            Some(Schedule::Hourly { minute: 20 })
        );
        assert_eq!(
            Schedule::of(&trigger(
                EventKind::Daily,
                &[(ATTR_HOUR, json!(3)), (ATTR_MINUTE, json!(30))]
            ))
            .unwrap(),
            Some(Schedule::Daily {
                hour: 3,
                minute: 30
            })
        );
        assert_eq!(
            Schedule::of(&trigger(
                EventKind::Weekly,
                &[
                    (ATTR_DAY_OF_WEEK, json!(6)),
                    (ATTR_HOUR, json!(9)),
                    (ATTR_MINUTE, json!(5))
                ]
            ))
            .unwrap(),
            Some(Schedule::Weekly {
                day_of_week: 6,
                hour: 9,
                minute: 5
            })
        );

        // Unset means 0: a `daily` nobody configured fires at midnight UTC,
        // rather than refusing to be a trigger.
        assert_eq!(
            Schedule::of(&trigger(EventKind::Daily, &[])).unwrap(),
            Some(Schedule::Daily { hour: 0, minute: 0 })
        );

        // A trigger that does not fire on a schedule has none.
        assert_eq!(Schedule::of(&trigger(EventKind::None, &[])).unwrap(), None);
        assert_eq!(
            Schedule::of(&trigger(EventKind::Insert, &[])).unwrap(),
            None
        );
    }

    #[test]
    fn timing_a_kind_does_not_use_is_refused_rather_than_ignored() {
        // The failure this prevents: an admin sets 03:00 on an `hourly` trigger,
        // saves it, and it goes on firing every hour at :00 with no sign that the
        // hour was dropped.
        let err = Schedule::of(&trigger(EventKind::Hourly, &[(ATTR_HOUR, json!(3))]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("hour"), "{err}");
        assert!(err.contains("hourly"), "{err}");

        let err = Schedule::of(&trigger(EventKind::Often, &[(ATTR_MINUTE, json!(3))]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("minute"), "{err}");

        let err = Schedule::of(&trigger(EventKind::Login, &[(ATTR_HOUR, json!(3))]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not fire on a schedule"), "{err}");

        // An explicit null is "not set", which is what a form posts for an input
        // it did not show.
        assert!(Schedule::of(&trigger(EventKind::Often, &[(ATTR_MINUTE, Json::Null)])).is_ok());
    }

    #[test]
    fn out_of_range_timing_is_refused_by_name() {
        for (kind, key, value) in [
            (EventKind::Hourly, ATTR_MINUTE, json!(60)),
            (EventKind::Daily, ATTR_HOUR, json!(24)),
            (EventKind::Weekly, ATTR_DAY_OF_WEEK, json!(7)),
            (EventKind::Hourly, ATTR_MINUTE, json!(-1)),
            (EventKind::Hourly, ATTR_MINUTE, json!("half past")),
            (EventKind::Daily, ATTR_HOUR, json!(2.5)),
        ] {
            let err = Schedule::of(&trigger(kind, &[(key, value.clone())]))
                .unwrap_err()
                .to_string();
            assert!(err.contains(key), "{key}={value}: {err}");
        }
    }

    #[test]
    fn next_due_over_the_rollovers() {
        // `often` is a spacing from the last run, not a wall-clock grid.
        assert_eq!(
            Schedule::Often.next_due(at("2026-07-25T10:03:20Z")),
            at("2026-07-25T10:08:20Z")
        );

        // Hourly: this hour if the minute is still ahead, else the next hour —
        // and the seconds of the last run are dropped, not carried for ever.
        let hourly = Schedule::Hourly { minute: 15 };
        assert_eq!(
            hourly.next_due(at("2026-07-25T10:05:30Z")),
            at("2026-07-25T10:15:00Z")
        );
        assert_eq!(
            hourly.next_due(at("2026-07-25T10:15:00Z")),
            at("2026-07-25T11:15:00Z")
        );
        // Across midnight.
        assert_eq!(
            hourly.next_due(at("2026-07-25T23:40:00Z")),
            at("2026-07-26T00:15:00Z")
        );

        // Daily, including the day rollover and the month's end.
        let daily = Schedule::Daily {
            hour: 3,
            minute: 30,
        };
        assert_eq!(
            daily.next_due(at("2026-07-25T01:00:00Z")),
            at("2026-07-25T03:30:00Z")
        );
        assert_eq!(
            daily.next_due(at("2026-07-25T03:30:00Z")),
            at("2026-07-26T03:30:00Z")
        );
        assert_eq!(
            daily.next_due(at("2026-07-31T09:00:00Z")),
            at("2026-08-01T03:30:00Z")
        );
        // And the year's.
        assert_eq!(
            daily.next_due(at("2026-12-31T23:59:00Z")),
            at("2027-01-01T03:30:00Z")
        );

        // Weekly: 2026-07-25 is a Saturday.
        assert_eq!(weekday_number(at("2026-07-25T00:00:00Z")), 5);
        let weekly = Schedule::Weekly {
            day_of_week: 0, // Monday
            hour: 6,
            minute: 0,
        };
        assert_eq!(
            weekly.next_due(at("2026-07-25T12:00:00Z")),
            at("2026-07-27T06:00:00Z")
        );
        // On the day but past the time: next week, not later today.
        assert_eq!(
            weekly.next_due(at("2026-07-27T06:00:00Z")),
            at("2026-08-03T06:00:00Z")
        );
        // On the day and still before the time: today.
        assert_eq!(
            weekly.next_due(at("2026-07-27T05:59:00Z")),
            at("2026-07-27T06:00:00Z")
        );
    }

    #[test]
    fn is_due_over_a_table_of_kinds_configurations_and_clocks() {
        // (schedule, last run, now, due?) — the whole decision the scheduler
        // makes, stated as data.
        let cases: &[(Schedule, &str, &str, bool)] = &[
            // `often`: five minutes from the last run, to the second.
            (
                Schedule::Often,
                "2026-07-25T10:00:00Z",
                "2026-07-25T10:04:59Z",
                false,
            ),
            (
                Schedule::Often,
                "2026-07-25T10:00:00Z",
                "2026-07-25T10:05:00Z",
                true,
            ),
            // A long gap is still one run, not one per elapsed period.
            (
                Schedule::Often,
                "2026-07-25T10:00:00Z",
                "2026-07-25T18:00:00Z",
                true,
            ),
            // Hourly at :15 — before it, on it, and after the hour rolls over.
            (
                Schedule::Hourly { minute: 15 },
                "2026-07-25T10:15:00Z",
                "2026-07-25T11:14:00Z",
                false,
            ),
            (
                Schedule::Hourly { minute: 15 },
                "2026-07-25T10:15:00Z",
                "2026-07-25T11:15:00Z",
                true,
            ),
            (
                Schedule::Hourly { minute: 0 },
                "2026-07-25T23:00:00Z",
                "2026-07-26T00:00:00Z",
                true,
            ),
            // Daily at 03:30, across the day boundary and the month's end.
            (
                Schedule::Daily {
                    hour: 3,
                    minute: 30,
                },
                "2026-07-25T03:30:00Z",
                "2026-07-26T03:29:00Z",
                false,
            ),
            (
                Schedule::Daily {
                    hour: 3,
                    minute: 30,
                },
                "2026-07-25T03:30:00Z",
                "2026-07-26T03:30:00Z",
                true,
            ),
            (
                Schedule::Daily { hour: 0, minute: 0 },
                "2026-07-31T00:00:00Z",
                "2026-08-01T00:00:00Z",
                true,
            ),
            // A trigger whose clock started this minute is never instantly due.
            (
                Schedule::Daily {
                    hour: 3,
                    minute: 30,
                },
                "2026-07-25T10:00:00Z",
                "2026-07-25T10:00:00Z",
                false,
            ),
            // Weekly on Monday 06:00 — 2026-07-25 is a Saturday.
            (
                Schedule::Weekly {
                    day_of_week: 0,
                    hour: 6,
                    minute: 0,
                },
                "2026-07-20T06:00:00Z",
                "2026-07-25T12:00:00Z",
                false,
            ),
            (
                Schedule::Weekly {
                    day_of_week: 0,
                    hour: 6,
                    minute: 0,
                },
                "2026-07-20T06:00:00Z",
                "2026-07-27T06:00:00Z",
                true,
            ),
            // Sunday, one minute before and one minute after — the week rolls
            // over at the day, not at the number.
            (
                Schedule::Weekly {
                    day_of_week: 6,
                    hour: 23,
                    minute: 59,
                },
                "2026-07-19T23:59:00Z",
                "2026-07-26T23:58:00Z",
                false,
            ),
            (
                Schedule::Weekly {
                    day_of_week: 6,
                    hour: 23,
                    minute: 59,
                },
                "2026-07-19T23:59:00Z",
                "2026-07-26T23:59:00Z",
                true,
            ),
        ];
        for (schedule, last_run, now, expected) in cases {
            assert_eq!(
                schedule.is_due(at(now), at(last_run)),
                *expected,
                "{schedule} last ran {last_run}, now {now}"
            );
        }
    }

    #[test]
    fn a_run_missed_while_the_server_was_down_is_due_exactly_once() {
        // Three days of downtime, a daily trigger: due now (so it runs once at
        // startup), and once it has run the next one is tomorrow — not three
        // catch-up runs, and not a lost one.
        let daily = Schedule::Daily { hour: 3, minute: 0 };
        let last_run = at("2026-07-22T03:00:00Z");
        let now = at("2026-07-25T09:12:00Z");
        assert!(daily.is_due(now, last_run));
        assert!(!daily.is_due(now, now));
        assert_eq!(daily.next_due(now), at("2026-07-26T03:00:00Z"));
    }

    #[test]
    fn it_describes_itself_for_the_admin() {
        assert_eq!(Schedule::Often.to_string(), "every 5 minutes");
        assert_eq!(Schedule::Hourly { minute: 5 }.to_string(), "hourly at :05");
        assert_eq!(
            Schedule::Daily {
                hour: 3,
                minute: 30
            }
            .to_string(),
            "daily at 03:30 UTC"
        );
        assert_eq!(
            Schedule::Weekly {
                day_of_week: 6,
                hour: 9,
                minute: 0
            }
            .to_string(),
            "weekly on Sunday at 09:00 UTC"
        );
    }
}
