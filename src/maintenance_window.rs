//! An optional maintenance window for `supervise`, so unattended overnight
//! batch runs can be confined to a time-of-day (and optionally day-of-week)
//! range without an operator wrapping the process in an external cron
//! start/stop that would leave a mailbox mid-run when the window closes.
//!
//! Parsing and window-membership are kept pure and independent of the wall
//! clock so the automation boundary is directly testable; only
//! `MaintenanceWindow::contains_now` touches real time.
use chrono::{Datelike, Local, Timelike, Weekday};

const USAGE: &str = "maintenance window must be HH:MM-HH:MM (24-hour, may wrap past midnight), optionally followed by @Mon,Tue,... to restrict it to specific days";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MaintenanceWindow {
    /// Minutes since local midnight, 0..1440.
    start_minute: u32,
    /// Minutes since local midnight, 0..1440. Equal to `start_minute` means
    /// a full 24-hour window (every minute matches); this is the only case
    /// where start == end is meaningful rather than an empty window.
    end_minute: u32,
    /// `None` means every day; `Some` restricts to the marked weekdays.
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

    /// True when `minute_of_day` (0..1440) on `weekday` falls inside the
    /// window. A window whose end is not after its start is treated as
    /// wrapping past midnight (for example `22:00-06:00`), matching either
    /// side of midnight; the calendar day used for the day-of-week check is
    /// always the day the minute falls on, not the day the window started.
    fn contains(&self, minute_of_day: u32, weekday: Weekday) -> bool {
        if let Some(days) = self.days
            && !days[weekday.num_days_from_monday() as usize]
        {
            return false;
        }
        if self.start_minute == self.end_minute {
            return true;
        }
        if self.start_minute < self.end_minute {
            (self.start_minute..self.end_minute).contains(&minute_of_day)
        } else {
            minute_of_day >= self.start_minute || minute_of_day < self.end_minute
        }
    }

    pub(crate) fn contains_now(&self) -> bool {
        let now = Local::now();
        let minute_of_day = now.hour() * 60 + now.minute();
        self.contains(minute_of_day, now.weekday())
    }
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
        days[weekday.num_days_from_monday() as usize] = true;
    }
    if days.iter().all(|&enabled| !enabled) {
        return Err(USAGE);
    }
    Ok(days)
}

fn parse_weekday(token: &str) -> Result<Weekday, &'static str> {
    let lower = token.to_ascii_lowercase();
    let prefix = lower.get(..3).unwrap_or(&lower);
    match prefix {
        "mon" => Ok(Weekday::Mon),
        "tue" => Ok(Weekday::Tue),
        "wed" => Ok(Weekday::Wed),
        "thu" => Ok(Weekday::Thu),
        "fri" => Ok(Weekday::Fri),
        "sat" => Ok(Weekday::Sat),
        "sun" => Ok(Weekday::Sun),
        _ => Err(USAGE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_same_day_window() {
        let window = MaintenanceWindow::parse("09:00-17:30").unwrap();
        assert!(window.contains(9 * 60, Weekday::Wed));
        assert!(window.contains(17 * 60 + 29, Weekday::Wed));
        assert!(!window.contains(17 * 60 + 30, Weekday::Wed));
        assert!(!window.contains(8 * 60 + 59, Weekday::Wed));
    }

    #[test]
    fn parses_a_window_that_wraps_past_midnight() {
        let window = MaintenanceWindow::parse("22:00-06:00").unwrap();
        assert!(window.contains(23 * 60, Weekday::Fri));
        assert!(window.contains(0, Weekday::Sat));
        assert!(window.contains(5 * 60 + 59, Weekday::Sat));
        assert!(!window.contains(6 * 60, Weekday::Sat));
        assert!(!window.contains(21 * 60 + 59, Weekday::Fri));
    }

    #[test]
    fn equal_start_and_end_means_every_minute() {
        let window = MaintenanceWindow::parse("00:00-00:00").unwrap();
        assert!(window.contains(0, Weekday::Mon));
        assert!(window.contains(23 * 60 + 59, Weekday::Sun));
    }

    #[test]
    fn restricts_to_configured_weekdays_on_both_sides_of_a_wrap() {
        let window = MaintenanceWindow::parse("22:00-06:00@Fri,Sat").unwrap();
        // The night of Friday-into-Saturday: 23:00 is "on" Friday.
        assert!(window.contains(23 * 60, Weekday::Fri));
        // 02:00 the following calendar day is "on" Saturday.
        assert!(window.contains(2 * 60, Weekday::Sat));
        assert!(!window.contains(23 * 60, Weekday::Sun));
        assert!(!window.contains(2 * 60, Weekday::Mon));
    }

    #[test]
    fn day_names_are_case_insensitive_and_accept_full_or_abbreviated_form() {
        let window = MaintenanceWindow::parse("00:00-00:00@monday,WED,Fri").unwrap();
        assert!(window.contains(0, Weekday::Mon));
        assert!(window.contains(0, Weekday::Wed));
        assert!(window.contains(0, Weekday::Fri));
        assert!(!window.contains(0, Weekday::Tue));
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
