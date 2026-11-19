//! An optional maintenance window for `supervise`, so unattended overnight
//! batch runs can be confined to a time-of-day (and optionally day-of-week)
//! range without an operator wrapping the process in an external cron
//! start/stop that would leave a mailbox mid-run when the window closes.
//!
//! A window is a wall-clock range in the host's local time. Each enabled
//! weekday opens one *occurrence*: `22:00-06:00@Fri` opens Friday at 22:00
//! and closes Saturday at 06:00. Membership and time-until-close are both
//! derived from that one occurrence, so they can never disagree, and the
//! close instant is resolved through the time zone so a window spanning a
//! daylight-saving change lasts its real wall-clock length.
//!
//! Everything except `contains_now`/`closes_in_now` is pure and takes the
//! instant to evaluate, so boundaries are directly testable.
use jiff::{
    Span, Timestamp, ZonedDateTime,
    civil::{Date, DateTime, Weekday},
    tz::TimeZone,
};

const USAGE: &str = "maintenance window must be HH:MM-HH:MM (24-hour, may wrap past midnight), optionally followed by @Mon,Tue,... naming the days each window opens";
const MINUTES_PER_DAY: i64 = 24 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MaintenanceWindow {
    /// Minutes since local midnight, 0..1440.
    start_minute: u32,
    /// Minutes since local midnight, 0..1440. Equal to `start_minute` means
    /// a full 24-hour window; this is the only case where start == end is
    /// meaningful rather than an empty window.
    end_minute: u32,
    /// `None` means every day; `Some` restricts which days a window opens.
    days: Option<[bool; 7]>,
}

impl MaintenanceWindow {
    pub(crate) fn parse(spec: &str) -> Result<Self, &'static str> {
        let spec = spec.trim();
        let (time_range, days_spec) = match spec.split_once('@') {
            Some((range, days)) => (range, Some(days)),
            None => (spec, None),
        };
        let (start, end) = time_range.split_once('-').ok_or(USAGE)?;
        let start_minute = parse_clock(start)?;
        let end_minute = parse_clock(end)?;
        let days = days_spec.map(parse_days).transpose()?;
        Ok(Self {
            start_minute,
            end_minute,
            days,
        })
    }

    fn opens_on(&self, weekday: Weekday) -> bool {
        self.days
            .is_none_or(|days| days[weekday.to_monday_zero_offset() as usize])
    }

    fn length_minutes(&self) -> i64 {
        let (start, end) = (i64::from(self.start_minute), i64::from(self.end_minute));
        if start == end {
            MINUTES_PER_DAY
        } else if end > start {
            end - start
        } else {
            MINUTES_PER_DAY - start + end
        }
    }

    /// Wall-clock start and end of the occurrence that opens on `date`.
    fn occurrence(&self, date: Date) -> Option<(DateTime, DateTime)> {
        let start = date
            .at(0, 0, 0, 0)
            .checked_add(Span::new().minutes(i64::from(self.start_minute)))
            .ok()?;
        let end = start
            .checked_add(Span::new().minutes(self.length_minutes()))
            .ok()?;
        Some((start, end))
    }

    /// Wall-clock close of the occurrence containing `local`: `None` outside
    /// every occurrence, `Some(None)` for a window that never closes. Back-to-
    /// back occurrences (full-day windows on consecutive days) are one span.
    fn close_at(&self, local: DateTime) -> Option<Option<DateTime>> {
        let today = local.date();
        let previous = today.checked_add(Span::new().days(-1)).ok()?;
        let (_, mut end) = [previous, today]
            .into_iter()
            .filter(|date| self.opens_on(date.weekday()))
            .filter_map(|date| self.occurrence(date).map(|occurrence| (date, occurrence)))
            .find(|(_, (start, end))| *start <= local && local < *end)?
            .1;
        // At most a week of chaining; a window that is still continuous after
        // that (every day, full-day) never closes.
        for _ in 0..7 {
            let next_date = end.date();
            let (next_start, next_end) = self.occurrence(next_date)?;
            if next_start != end || !self.opens_on(next_date.weekday()) {
                return Some(Some(end));
            }
            end = next_end;
        }
        Some(None)
    }

    fn contains_at(&self, now: &ZonedDateTime) -> bool {
        self.closes_in_at(now).is_some()
    }

    /// Time until the window closes, `None` outside it, and `Some(None)` for
    /// a window that never closes. An occurrence whose resolved close has
    /// already passed is outside: in a repeated fall-back hour the second
    /// 02:15 reads as before a 02:30 close that happened an hour ago.
    fn closes_in_at(&self, now: &ZonedDateTime) -> Option<Option<std::time::Duration>> {
        match self.close_at(now.datetime())? {
            None => Some(None),
            Some(close) => {
                let close = resolve_local(now.time_zone(), close)?;
                let remaining =
                    std::time::Duration::try_from(now.timestamp().duration_until(close)).ok()?;
                (!remaining.is_zero()).then_some(Some(remaining))
            }
        }
    }

    pub(crate) fn closes_in_now(&self) -> Option<Option<std::time::Duration>> {
        self.closes_in_at(&ZonedDateTime::now())
    }

    pub(crate) fn contains_now(&self) -> bool {
        self.contains_at(&ZonedDateTime::now())
    }
}

/// The instant a wall-clock time denotes. A time repeated by a fall-back
/// change resolves to its first occurrence (the window closes early rather
/// than late); a time skipped by a spring-forward change resolves to the
/// moment the clock jumps past it.
fn resolve_local(zone: &TimeZone, local: DateTime) -> Option<Timestamp> {
    let mut candidate = local;
    for _ in 0..=MINUTES_PER_DAY {
        let instant = candidate.to_zoned_datetime(zone.clone()).ok()?;
        if instant.datetime() == candidate {
            return Some(instant.timestamp());
        }
        candidate = candidate.checked_add(Span::new().minutes(1)).ok()?;
    }
    None
}

fn parse_clock(value: &str) -> Result<u32, &'static str> {
    let (hours, minutes) = value.trim().split_once(':').ok_or(USAGE)?;
    let hours: u32 = hours.parse().map_err(|_| USAGE)?;
    let minutes: u32 = minutes.parse().map_err(|_| USAGE)?;
    if hours > 23 || minutes > 59 {
        return Err(USAGE);
    }
    Ok(hours * 60 + minutes)
}

fn parse_days(spec: &str) -> Result<[bool; 7], &'static str> {
    let mut days = [false; 7];
    for token in spec.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return Err(USAGE);
        }
        let weekday = parse_weekday(token)?;
        days[weekday.to_monday_zero_offset() as usize] = true;
    }
    if days.iter().all(|&enabled| !enabled) {
        return Err(USAGE);
    }
    Ok(days)
}

/// Exactly a three-letter abbreviation or the full English day name, in any
/// case; anything else (`Mo`, `MondayXYZ`) is a configuration error.
fn parse_weekday(token: &str) -> Result<Weekday, &'static str> {
    const NAMES: [(&str, &str, Weekday); 7] = [
        ("mon", "monday", Weekday::Monday),
        ("tue", "tuesday", Weekday::Tuesday),
        ("wed", "wednesday", Weekday::Wednesday),
        ("thu", "thursday", Weekday::Thursday),
        ("fri", "friday", Weekday::Friday),
        ("sat", "saturday", Weekday::Saturday),
        ("sun", "sunday", Weekday::Sunday),
    ];
    let lower = token.to_ascii_lowercase();
    NAMES
        .iter()
        .find(|(short, long, _)| lower == *short || lower == *long)
        .map(|(_, _, weekday)| *weekday)
        .ok_or(USAGE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::{SignedDuration, civil::date};

    /// 2026-10-23 is a Friday.
    fn at(day: i8, hour: i8, minute: i8) -> ZonedDateTime {
        date(2026, 10, day)
            .at(hour, minute, 0, 0)
            .to_zoned_datetime(TimeZone::UTC)
            .unwrap()
    }

    fn contains(spec: &str, instant: ZonedDateTime) -> bool {
        MaintenanceWindow::parse(spec)
            .unwrap()
            .contains_at(&instant)
    }

    fn closes_in_minutes(spec: &str, instant: ZonedDateTime) -> Option<Option<u64>> {
        MaintenanceWindow::parse(spec)
            .unwrap()
            .closes_in_at(&instant)
            .map(|close| close.map(|duration| duration.as_secs() / 60))
    }

    #[test]
    fn parses_a_same_day_window() {
        let spec = "09:00-17:30";
        assert!(contains(spec, at(21, 9, 0)));
        assert!(contains(spec, at(21, 17, 29)));
        assert!(!contains(spec, at(21, 17, 30)));
        assert!(!contains(spec, at(21, 8, 59)));
        assert_eq!(closes_in_minutes(spec, at(21, 16, 30)), Some(Some(60)));
    }

    #[test]
    fn a_wrapped_window_spans_midnight() {
        let spec = "22:00-06:00";
        assert!(contains(spec, at(23, 23, 0)));
        assert!(contains(spec, at(24, 0, 0)));
        assert!(contains(spec, at(24, 5, 59)));
        assert!(!contains(spec, at(24, 6, 0)));
        assert!(!contains(spec, at(23, 21, 59)));
        assert_eq!(closes_in_minutes(spec, at(23, 23, 0)), Some(Some(420)));
        assert_eq!(closes_in_minutes(spec, at(24, 5, 0)), Some(Some(60)));
    }

    #[test]
    fn a_weekday_names_the_day_the_window_opens() {
        // External review finding 1: Friday night must stay open into
        // Saturday morning, and the remaining time must match membership.
        let spec = "22:00-06:00@Fri";
        assert!(contains(spec, at(23, 23, 0)));
        assert_eq!(closes_in_minutes(spec, at(23, 23, 0)), Some(Some(420)));
        assert!(contains(spec, at(24, 0, 30)));
        assert_eq!(closes_in_minutes(spec, at(24, 0, 30)), Some(Some(330)));
        assert!(!contains(spec, at(24, 6, 0)));
        // Saturday night is not a Friday window, and neither is the tail of
        // Thursday night on Friday morning.
        assert!(!contains(spec, at(24, 23, 0)));
        assert!(!contains(spec, at(23, 2, 0)));
        assert!(!contains(spec, at(22, 23, 0)));
    }

    #[test]
    fn membership_and_time_to_close_always_agree() {
        for spec in [
            "09:00-17:00",
            "22:00-06:00",
            "22:00-06:00@Fri",
            "22:00-06:00@Fri,Sat",
            "23:30-00:15@Sun,Mon",
            "00:00-00:00@Mon,Tue",
            "18:00-18:00@Wed",
        ] {
            let window = MaintenanceWindow::parse(spec).unwrap();
            let mut instant = at(19, 0, 0);
            let end = at(19, 0, 0)
                .checked_add(SignedDuration::from_hours(14 * 24))
                .unwrap();
            while instant < end {
                let inside = window.contains_at(&instant);
                let closes = window.closes_in_at(&instant);
                assert_eq!(inside, closes.is_some(), "{spec} at {instant}");
                if let Some(Some(remaining)) = closes {
                    let remaining = SignedDuration::try_from(remaining).unwrap();
                    let last = instant
                        .checked_add(remaining)
                        .unwrap()
                        .checked_add(jiff::SignedDuration::from_mins(-1))
                        .unwrap();
                    assert!(window.contains_at(&last), "{spec}: open until {last}");
                    assert!(
                        !window.contains_at(&instant.checked_add(remaining).unwrap()),
                        "{spec}: closed at {}",
                        instant.checked_add(remaining).unwrap()
                    );
                }
                instant = instant
                    .checked_add(jiff::SignedDuration::from_mins(15))
                    .unwrap();
            }
        }
    }

    #[test]
    fn full_day_windows_chain_across_consecutive_days() {
        // Monday noon to the end of Tuesday is one continuous span.
        assert_eq!(
            closes_in_minutes("00:00-00:00@Mon,Tue", at(19, 12, 0)),
            Some(Some(36 * 60))
        );
        assert_eq!(closes_in_minutes("00:00-00:00", at(21, 12, 0)), Some(None));
        assert_eq!(
            closes_in_minutes("00:00-00:00@Mon,Tue,Wed,Thu,Fri,Sat,Sun", at(21, 12, 0)),
            Some(None)
        );
        assert!(!contains("00:00-00:00@Mon,Tue", at(21, 12, 0)));
    }

    fn cet(month: i8, day: i8, hour: i8, minute: i8) -> ZonedDateTime {
        date(2026, month, day)
            .at(hour, minute, 0, 0)
            .to_zoned_datetime(TimeZone::get("Europe/Amsterdam").unwrap())
            .unwrap()
    }

    fn utc_to_cet(month: i8, day: i8, hour: i8, minute: i8) -> ZonedDateTime {
        date(2026, month, day)
            .at(hour, minute, 0, 0)
            .to_zoned_datetime(TimeZone::UTC)
            .unwrap()
            .timestamp()
            .to_zoned_datetime(TimeZone::get("Europe/Amsterdam").unwrap())
    }

    fn cet_closes_in(spec: &str, now: &ZonedDateTime) -> Option<Option<u64>> {
        MaintenanceWindow::parse(spec)
            .unwrap()
            .closes_in_at(now)
            .map(|close| close.map(|duration| duration.as_secs() / 60))
    }

    #[test]
    fn a_window_across_a_daylight_saving_change_lasts_its_real_length() {
        // Spring forward: 22:00-06:00 on 28/29 March is only seven hours.
        assert_eq!(
            cet_closes_in("22:00-06:00", &cet(3, 28, 23, 0)),
            Some(Some(6 * 60))
        );
        // Fall back: 22:00-06:00 on 24/25 October is nine hours.
        assert_eq!(
            cet_closes_in("22:00-06:00", &cet(10, 24, 23, 0)),
            Some(Some(8 * 60))
        );
        // A close time that does not exist (02:30 is skipped) closes when the
        // clock jumps past it.
        assert_eq!(
            cet_closes_in("22:00-02:30", &cet(3, 28, 23, 0)),
            Some(Some(3 * 60))
        );
        // A close time that occurs twice closes at its first occurrence.
        assert_eq!(
            cet_closes_in("22:00-02:30", &cet(10, 24, 23, 0)),
            Some(Some(3 * 60 + 30))
        );
    }

    #[test]
    fn the_repeated_fall_back_hour_after_the_close_is_outside_the_window() {
        let window = MaintenanceWindow::parse("22:00-02:30").unwrap();
        // 00:15 UTC is the first 02:15 (summer time), before the close.
        let first = utc_to_cet(10, 25, 0, 15);
        assert!(window.contains_at(&first));
        assert_eq!(cet_closes_in("22:00-02:30", &first), Some(Some(15)));
        // 01:15 UTC is the second 02:15 (winter time), after the close at
        // the first 02:30: membership and time-to-close both say closed.
        let second = utc_to_cet(10, 25, 1, 15);
        assert_eq!(second.datetime(), first.datetime());
        assert!(!window.contains_at(&second));
        assert_eq!(cet_closes_in("22:00-02:30", &second), None);
    }

    #[test]
    fn day_names_must_be_exact_abbreviations_or_full_names() {
        let window = MaintenanceWindow::parse("00:00-00:00@monday,WED,Fri").unwrap();
        assert!(window.contains_at(&at(19, 0, 0)));
        assert!(window.contains_at(&at(21, 0, 0)));
        assert!(window.contains_at(&at(23, 0, 0)));
        assert!(!window.contains_at(&at(20, 12, 0)));
        // External review finding 2: prefixes alone are not day names.
        for spec in [
            "09:00-17:00@MondayXYZ",
            "09:00-17:00@tuesdaybroken",
            "09:00-17:00@FridayWhatever",
            "09:00-17:00@Mo",
            "09:00-17:00@Fr",
        ] {
            assert!(MaintenanceWindow::parse(spec).is_err(), "{spec}");
        }
    }

    #[test]
    fn rejects_malformed_specs() {
        for spec in [
            "",
            "9:00",
            "9:00-17:00-20:00",
            "24:00-06:00",
            "09:60-17:00",
            "09:00-17:00@",
            "09:00-17:00@Notaday",
            "abc-def",
        ] {
            assert!(
                MaintenanceWindow::parse(spec).is_err(),
                "expected {spec:?} to be rejected"
            );
        }
    }
}
