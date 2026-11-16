//! The week's clock (COMPETITION.md#the-week): every time a week runs on,
//! in Israel time and following its daylight saving, from `config.yml`.
//! The contracts see only the timestamps this gives (unix seconds).
//!
//! - **Open:** Sunday 12:00, the field's freeze. Never earlier: Saturday
//!   night is still Shabbat in the Americas.
//! - **League slots:** each league day (Sunday to Thursday) has
//!   `rounds_per_day` slots, ending every `24h / rounds_per_day` and the last
//!   at 21:00. A slot is a close window of `window` seconds ending then: its
//!   round's sessions close staggered across it. A slot before the open, or
//!   whose window starts too soon to post into, isn't used.
//! - **Pool close:** the last slot's end on Thursday (21:00), committed in
//!   the field entry whether or not that round is reached.
//! - **Final close:** Friday 12:00. The final is posted at least an hour
//!   before it, by 11:00.
//!
//! **The devnet's compressed week** (OPERATIONS.md#the-devnet): with
//! `WEEK_DAY_SECONDS` set, the same calendar runs on a virtual clock whose
//! day lasts that many real seconds, so a whole week, freeze to final, takes
//! hours. Every time it gives is still a real unix time. What the contracts
//! and Base fix stays in real seconds: a round's close window, the floor from
//! a post to its close.
//!
//! **A compressed clock can be moved** ([`WeekClock::phased_at`]): the
//! devnet's admin action that opens a week at once shifts the virtual
//! clock so an open falls at that instant, and later weeks follow from it. A
//! week's own times are always worked out on the clock phased at its own
//! open, so moving the clock never moves a week that has opened. The
//! calendar clock can't be moved: prod's and the testnet's weeks open when
//! the calendar says.

use jiff::civil::{Date, Time, Weekday};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};

pub const VAR_TIMEZONE: &str = "WEEK_TIMEZONE";
pub const VAR_OPEN_DAY: &str = "WEEK_OPEN_DAY";
pub const VAR_OPEN_TIME: &str = "WEEK_OPEN_TIME";
pub const VAR_ROUND_CLOSE_END: &str = "WEEK_ROUND_CLOSE_END";
pub const VAR_LAST_ROUND_DAY: &str = "WEEK_LAST_ROUND_DAY";
pub const VAR_FINAL_DAY: &str = "WEEK_FINAL_DAY";
pub const VAR_FINAL_CLOSE: &str = "WEEK_FINAL_CLOSE";
/// Optional: the compressed week's day, in real seconds.
pub const VAR_DAY_SECONDS: &str = "WEEK_DAY_SECONDS";

/// Where the compressed week's virtual calendar starts: Sunday 2026-11-01
/// 00:00 UTC, which is also real time there.
const VIRTUAL_ORIGIN: u64 = 1_793_491_200;

/// Every var the clock reads, for the taskfiles' lists.
pub const VARS: [&str; 7] = [
    VAR_TIMEZONE,
    VAR_OPEN_DAY,
    VAR_OPEN_TIME,
    VAR_ROUND_CLOSE_END,
    VAR_LAST_ROUND_DAY,
    VAR_FINAL_DAY,
    VAR_FINAL_CLOSE,
];

#[derive(Debug, Clone)]
pub struct WeekClock {
    tz: TimeZone,
    open_day: Weekday,
    open_time: Time,
    round_close_end: Time,
    last_round_day: Weekday,
    final_day: Weekday,
    final_close: Time,
    /// The compressed week's day in real seconds; `None` is the calendar.
    day_s: Option<u64>,
    /// How far a compressed clock is moved, in real seconds: its virtual
    /// calendar starts this long after [`VIRTUAL_ORIGIN`]. Always zero on
    /// the calendar.
    phase_s: u64,
}

/// One league slot: its close window, in unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub start: u64,
    pub end: u64,
}

fn weekday(name: &str) -> anyhow::Result<Weekday> {
    Ok(match name.trim().to_ascii_lowercase().as_str() {
        "monday" => Weekday::Monday,
        "tuesday" => Weekday::Tuesday,
        "wednesday" => Weekday::Wednesday,
        "thursday" => Weekday::Thursday,
        "friday" => Weekday::Friday,
        "saturday" => Weekday::Saturday,
        "sunday" => Weekday::Sunday,
        other => anyhow::bail!("not a weekday: {other}"),
    })
}

fn time(hhmm: &str) -> anyhow::Result<Time> {
    let (h, m) = hhmm
        .trim()
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("a time is HH:MM: {hhmm}"))?;
    Ok(Time::new(h.parse()?, m.parse()?, 0, 0)?)
}

/// Days from `from` forward to `to` (0..=6).
fn days_between(from: Weekday, to: Weekday) -> i64 {
    i64::from(to.to_monday_zero_offset() - from.to_monday_zero_offset()).rem_euclid(7)
}

impl WeekClock {
    pub fn new(
        timezone: &str,
        open_day: &str,
        open_time: &str,
        round_close_end: &str,
        last_round_day: &str,
        final_day: &str,
        final_close: &str,
    ) -> anyhow::Result<Self> {
        let clock = Self {
            tz: TimeZone::get(timezone)?,
            open_day: weekday(open_day)?,
            open_time: time(open_time)?,
            round_close_end: time(round_close_end)?,
            last_round_day: weekday(last_round_day)?,
            final_day: weekday(final_day)?,
            final_close: time(final_close)?,
            day_s: None,
            phase_s: 0,
        };
        anyhow::ensure!(
            days_between(clock.open_day, clock.last_round_day)
                < days_between(clock.open_day, clock.final_day),
            "the final's day comes after the last league day"
        );
        Ok(clock)
    }

    /// From a Worker's vars (config.yml); `None` if any is missing. A
    /// compressed day (`WEEK_DAY_SECONDS`) is refused on a target whose weeks
    /// run on the calendar (`calendar`: the testnet and prod), as the
    /// devnet's durations are.
    pub fn from_vars(
        var: impl Fn(&str) -> Option<String>,
        calendar: bool,
    ) -> anyhow::Result<Option<Self>> {
        let Some(values) = VARS.iter().map(|v| var(v)).collect::<Option<Vec<String>>>() else {
            return Ok(None);
        };
        let clock = Self::new(
            &values[0], &values[1], &values[2], &values[3], &values[4], &values[5], &values[6],
        )?;
        Ok(Some(
            match var(VAR_DAY_SECONDS).filter(|v| !v.trim().is_empty()) {
                Some(_) if calendar => anyhow::bail!(
                    "{VAR_DAY_SECONDS} compresses the week, which the testnet and prod never do"
                ),
                Some(day) => clock.compressed(day.trim().parse()?)?,
                None => clock,
            },
        ))
    }

    /// The same week on a virtual clock whose day lasts `day_s` real
    /// seconds, in UTC (no daylight saving to skip a virtual hour).
    pub fn compressed(mut self, day_s: u64) -> anyhow::Result<Self> {
        anyhow::ensure!(day_s >= 60, "a compressed day is at least a minute");
        self.day_s = Some(day_s);
        self.tz = TimeZone::UTC;
        Ok(self)
    }

    /// Whether this is a compressed clock, the only kind that can be moved.
    pub fn is_compressed(&self) -> bool {
        self.day_s.is_some()
    }

    /// This clock moved so that a week opens exactly at `open` (real
    /// seconds), later weeks following one period apart. The calendar clock
    /// is returned as it is.
    pub fn phased_at(&self, open: u64) -> Self {
        let Some(day) = self.day_s else {
            return self.clone();
        };
        let mut unmoved = self.clone();
        unmoved.phase_s = 0;
        // Where the unmoved clock's own open falls, and how far `open` is
        // past it: less than one week period.
        let behind = open.saturating_sub(unmoved.open_of(open)) % (7 * day);
        unmoved.phase_s = behind;
        unmoved
    }

    /// Real seconds to the clock's own (virtual, when compressed).
    fn inner(&self, real: u64) -> u64 {
        match self.day_s {
            None => real,
            Some(day) => {
                let since = real.saturating_sub(VIRTUAL_ORIGIN + self.phase_s) as u128;
                VIRTUAL_ORIGIN + (since * 86_400 / day as u128) as u64
            }
        }
    }

    /// The clock's own seconds back to real ones, rounded up, so the real
    /// instant of an open maps back to that open, never just before it.
    fn outer(&self, own: u64) -> u64 {
        match self.day_s {
            None => own,
            Some(day) => {
                let since = own.saturating_sub(VIRTUAL_ORIGIN) as u128;
                VIRTUAL_ORIGIN + self.phase_s + (since * day as u128).div_ceil(86_400) as u64
            }
        }
    }

    /// Israel's week, as config.yml sets it (tests and local tools).
    pub fn israel() -> Self {
        Self::new(
            "Asia/Jerusalem",
            "Sunday",
            "12:00",
            "21:00",
            "Thursday",
            "Friday",
            "12:00",
        )
        .expect("the default week")
    }

    fn at(&self, date: Date, t: Time) -> u64 {
        // A local time skipped by a clock change resolves to the instant
        // after it; none of the configured times fall in one.
        date.to_datetime(t)
            .to_zoned(self.tz.clone())
            .map(|z| z.timestamp().as_second() as u64)
            .unwrap_or(0)
    }

    fn local_date(&self, s: u64) -> Date {
        Timestamp::from_second(s as i64)
            .unwrap_or(Timestamp::UNIX_EPOCH)
            .to_zoned(self.tz.clone())
            .date()
    }

    fn plus_days(date: Date, days: i64) -> Date {
        date.checked_add(Span::new().days(days)).unwrap_or(date)
    }

    fn open_date(&self, open: u64) -> Date {
        self.local_date(open)
    }

    /// A span the brief gives on the calendar (OPERATIONS.md#alerts' "two
    /// hours before the final's posting cutoff", say) on this clock: as is
    /// on the calendar, shrunk with the day on a compressed week.
    pub fn scaled(&self, calendar_s: u64) -> u64 {
        match self.day_s {
            Some(day) => calendar_s.saturating_mul(day) / 86_400,
            None => calendar_s,
        }
    }

    /// The most recent open at or before `now`.
    pub fn open_of(&self, now: u64) -> u64 {
        self.outer(self.open_of_own(self.inner(now)))
    }

    fn open_of_own(&self, now: u64) -> u64 {
        let today = self.local_date(now);
        let back = days_between(self.open_day, today.weekday());
        let date = Self::plus_days(today, -back);
        let open = self.at(date, self.open_time);
        if open <= now {
            open
        } else {
            self.at(Self::plus_days(date, -7), self.open_time)
        }
    }

    /// The first open after `now`.
    pub fn next_open(&self, now: u64) -> u64 {
        let this = self.open_of_own(self.inner(now));
        self.outer(self.at(Self::plus_days(self.open_date(this), 7), self.open_time))
    }

    /// Every league slot of the week opening at `open`, in order: each day
    /// from the open's to the last league day, `rounds_per_day` a day, ending
    /// every `24h / rounds_per_day` up to the round close end, each a window
    /// of `window_s`. Slots whose window starts before the open are left out.
    pub fn slots(&self, open: u64, rounds_per_day: u64, window_s: u64) -> Vec<Slot> {
        let per_day = rounds_per_day.max(1);
        let spacing = 86_400 / per_day;
        let first = self.open_date(self.inner(open));
        let days = days_between(self.open_day, self.last_round_day);
        let mut out = Vec::new();
        for d in 0..=days {
            let last_end = self.at(Self::plus_days(first, d), self.round_close_end);
            for i in 0..per_day {
                // Each end on the clock's own days; the window in real
                // seconds, as the contract and Base see it.
                let end = self.outer(last_end - (per_day - 1 - i) * spacing);
                let start = end.saturating_sub(window_s);
                if start >= open {
                    out.push(Slot { start, end });
                }
            }
        }
        out
    }

    /// The week-winner pool's close: the last league day's round close end.
    pub fn pool_close(&self, open: u64) -> u64 {
        let days = days_between(self.open_day, self.last_round_day);
        self.outer(self.at(
            Self::plus_days(self.open_date(self.inner(open)), days),
            self.round_close_end,
        ))
    }

    /// The final's close.
    pub fn final_close(&self, open: u64) -> u64 {
        let days = days_between(self.open_day, self.final_day);
        self.outer(self.at(
            Self::plus_days(self.open_date(self.inner(open)), days),
            self.final_close,
        ))
    }

    /// The latest the final can be posted: the contract's floor from a post
    /// to its close (`lead_s`, an hour in prod) before its close.
    pub fn final_posting_cutoff(&self, open: u64, lead_s: u64) -> u64 {
        self.final_close(open) - lead_s
    }

    /// Local wall-clock time of an instant, `YYYY-MM-DD HH:MM` (for logs and
    /// tests).
    pub fn local(&self, s: u64) -> String {
        let shown = Timestamp::from_second(self.inner(s) as i64)
            .unwrap_or(Timestamp::UNIX_EPOCH)
            .to_zoned(self.tz.clone())
            .strftime("%Y-%m-%d %H:%M %a")
            .to_string();
        match self.day_s {
            None => shown,
            Some(day) => format!("{shown} (compressed: a day is {day}s)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> u64 {
        // Local Israel time.
        let dt: jiff::civil::DateTime = s.parse().unwrap();
        dt.to_zoned(TimeZone::get("Asia/Jerusalem").unwrap())
            .unwrap()
            .timestamp()
            .as_second() as u64
    }

    #[test]
    fn a_week_opens_sunday_noon_and_its_final_closes_friday_noon() {
        let c = WeekClock::israel();
        // Wednesday 2026-11-11 10:00 is in the week that opened Sunday the 8th.
        let open = c.open_of(ts("2026-11-11T10:00"));
        assert_eq!(c.local(open), "2026-11-08 12:00 Sun");
        assert_eq!(c.local(c.pool_close(open)), "2026-11-12 21:00 Thu");
        assert_eq!(c.local(c.final_close(open)), "2026-11-13 12:00 Fri");
        assert_eq!(
            c.local(c.final_posting_cutoff(open, 3600)),
            "2026-11-13 11:00 Fri"
        );
        assert_eq!(c.local(c.next_open(open)), "2026-11-15 12:00 Sun");
        // Sunday 11:59 still belongs to the week before; 12:00 opens the next.
        assert_eq!(
            c.local(c.open_of(ts("2026-11-15T11:59"))),
            "2026-11-08 12:00 Sun"
        );
        assert_eq!(
            c.local(c.open_of(ts("2026-11-15T12:00"))),
            "2026-11-15 12:00 Sun"
        );
        // Saturday night is still the old week: nothing opens before Sunday.
        assert_eq!(
            c.local(c.open_of(ts("2026-11-14T23:00"))),
            "2026-11-08 12:00 Sun"
        );
    }

    #[test]
    fn one_round_a_day_closes_at_21_00_sunday_to_thursday() {
        let c = WeekClock::israel();
        let open = ts("2026-11-08T12:00");
        let slots = c.slots(open, 1, 4 * 3600);
        let ends: Vec<String> = slots.iter().map(|s| c.local(s.end)).collect();
        assert_eq!(
            ends,
            vec![
                "2026-11-08 21:00 Sun",
                "2026-11-09 21:00 Mon",
                "2026-11-10 21:00 Tue",
                "2026-11-11 21:00 Wed",
                "2026-11-12 21:00 Thu",
            ]
        );
        assert_eq!(c.local(slots[0].start), "2026-11-08 17:00 Sun");
        // Two a day: 09:00 and 21:00, Sunday's morning slot before the open.
        let two = c.slots(open, 2, 3600);
        assert_eq!(two.len(), 9);
        assert_eq!(c.local(two[1].end), "2026-11-09 09:00 Mon");
    }

    #[test]
    fn times_hold_in_local_time_across_a_daylight_saving_change() {
        let c = WeekClock::israel();
        // Israel's 2026 autumn change: the last Sunday in October, 25 Oct,
        // at 02:00 back to 01:00. The week opening that day keeps noon.
        let open = c.open_of(ts("2026-10-27T09:00"));
        assert_eq!(c.local(open), "2026-10-25 12:00 Sun");
        assert_eq!(c.local(c.final_close(open)), "2026-10-30 12:00 Fri");
        // Spring forward, the Friday before the last Sunday in March (27
        // March 2026): that week's final still closes at noon local time.
        let spring = c.open_of(ts("2026-03-25T09:00"));
        assert_eq!(c.local(spring), "2026-03-22 12:00 Sun");
        assert_eq!(c.local(c.final_close(spring)), "2026-03-27 12:00 Fri");
        // And it's an hour less in UTC than the week before's.
        let before = c.open_of(ts("2026-03-18T09:00"));
        assert_eq!(
            c.final_close(spring) - c.final_close(before),
            7 * 86_400 - 3600
        );
    }

    #[test]
    fn a_configuration_is_checked() {
        assert!(
            WeekClock::new(
                "Mars/Olympus",
                "Sunday",
                "12:00",
                "21:00",
                "Thursday",
                "Friday",
                "12:00"
            )
            .is_err()
        );
        assert!(
            WeekClock::new(
                "Asia/Jerusalem",
                "Sunday",
                "12:00",
                "21:00",
                "Friday",
                "Thursday",
                "12:00"
            )
            .is_err()
        );
        assert!(
            WeekClock::new(
                "Asia/Jerusalem",
                "Funday",
                "12:00",
                "21:00",
                "Thursday",
                "Friday",
                "12:00"
            )
            .is_err()
        );
    }

    /// The devnet's week: the same calendar, a day of two hours.
    #[test]
    fn a_compressed_week_runs_the_same_calendar_in_hours() {
        let day = 7_200;
        let c = WeekClock::new(
            "Asia/Jerusalem",
            "Sunday",
            "12:00",
            "12:00",
            "Thursday",
            "Friday",
            "12:00",
        )
        .unwrap()
        .compressed(day)
        .unwrap();
        let now = 1_800_000_000;
        let open = c.open_of(now);
        assert!(open <= now && now < c.next_open(now));
        // An open is its own week's, not the one before.
        assert_eq!(c.open_of(open), open);
        assert_eq!(c.next_open(now) - open, 7 * day);
        assert!(c.local(open).contains("12:00 Sun"), "{}", c.local(open));
        // Monday to Thursday at virtual noon: four league rounds, the
        // Sunday slot falling before the open; windows in real seconds.
        let slots = c.slots(open, 1, 600);
        assert_eq!(slots.len(), 4);
        for (i, s) in slots.iter().enumerate() {
            assert_eq!(s.end - open, (i as u64 + 1) * day);
            assert_eq!(s.end - s.start, 600);
        }
        assert_eq!(c.pool_close(open), slots[3].end);
        assert_eq!(c.final_close(open) - c.pool_close(open), day);
        assert_eq!(c.final_posting_cutoff(open, 600), c.final_close(open) - 600);
    }

    #[test]
    fn the_calendar_week_ignores_compression_unless_asked() {
        let vars = |extra: Option<&'static str>| {
            move |name: &str| match name {
                VAR_TIMEZONE => Some("Asia/Jerusalem".to_string()),
                VAR_OPEN_DAY => Some("Sunday".into()),
                VAR_OPEN_TIME => Some("12:00".into()),
                VAR_ROUND_CLOSE_END => Some("21:00".into()),
                VAR_LAST_ROUND_DAY => Some("Thursday".into()),
                VAR_FINAL_DAY => Some("Friday".into()),
                VAR_FINAL_CLOSE => Some("12:00".into()),
                VAR_DAY_SECONDS => extra.map(str::to_string),
                _ => None,
            }
        };
        let calendar = WeekClock::from_vars(vars(None), true).unwrap().unwrap();
        let fast = WeekClock::from_vars(vars(Some("7200")), false)
            .unwrap()
            .unwrap();
        // Prod never runs a compressed week.
        assert!(WeekClock::from_vars(vars(Some("7200")), true).is_err());
        let now = 1_800_000_000;
        assert_eq!(calendar.open_of(now), WeekClock::israel().open_of(now));
        assert_eq!(fast.next_open(now) - fast.open_of(now), 7 * 7_200);
    }

    /// The devnet's week, and the admin action that opens one at
    /// once: the clock moved to any instant opens a week exactly then.
    #[test]
    fn a_compressed_clock_moved_to_an_instant_opens_a_week_exactly_then() {
        let day = 1_800;
        let base = WeekClock::new(
            "Asia/Jerusalem",
            "Sunday",
            "12:00",
            "12:00",
            "Thursday",
            "Friday",
            "12:00",
        )
        .unwrap()
        .compressed(day)
        .unwrap();
        for now in [
            1_800_000_000u64,
            1_800_000_001,
            1_800_004_321,
            1_800_012_599,
        ] {
            let c = base.phased_at(now);
            assert_eq!(c.open_of(now), now, "an open falls at {now}");
            assert_eq!(c.open_of(now + 7 * day - 1), now);
            assert_eq!(c.next_open(now), now + 7 * day);
            assert_eq!(c.open_of(now - 1), now - 7 * day);
            // The week keeps its shape: four rounds half an hour apart, the
            // final half an hour after the last.
            let slots = c.slots(now, 1, 300);
            assert_eq!(slots.len(), 4);
            for (i, s) in slots.iter().enumerate() {
                assert_eq!(s.end - now, (i as u64 + 1) * day);
            }
            assert_eq!(c.pool_close(now), now + 4 * day);
            assert_eq!(c.final_close(now), now + 5 * day);
            // Moving it again to one of its own opens changes nothing, and a
            // week's times come out the same from any later phase of it.
            let again = c.phased_at(now + 7 * day);
            assert_eq!(again.slots(now, 1, 300), slots);
            assert_eq!(again.open_of(now), now);
        }
        // A week that opened on the unmoved clock keeps its times when read
        // through the clock phased at its own open, wherever the clock has
        // since been moved.
        let old_open = base.open_of(1_800_000_000);
        let moved = base.phased_at(1_800_004_321);
        assert_eq!(
            moved.phased_at(old_open).slots(old_open, 1, 300),
            base.slots(old_open, 1, 300)
        );
        // The calendar can't be moved.
        let cal = WeekClock::israel();
        assert!(!cal.is_compressed());
        assert_eq!(
            cal.phased_at(1_800_004_321).open_of(1_800_004_321),
            cal.open_of(1_800_004_321)
        );
    }

    #[test]
    fn a_calendar_span_shrinks_with_a_compressed_day() {
        assert_eq!(WeekClock::israel().scaled(7_200), 7_200);
        let fast = WeekClock::israel().compressed(8_640).unwrap();
        assert_eq!(fast.scaled(7_200), 720);
    }
}
