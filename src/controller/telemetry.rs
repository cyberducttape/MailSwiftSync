//! Runtime telemetry for the Activity operations view.
//!
//! Everything here is derived from content-free worker events: engine
//! progress counters, scheduled retries, and shared provider cooldowns. It is
//! presentation state for the current run only, never durable evidence, and
//! every estimate it produces is labelled as such by the view.

use crate::progress::TransferProgress;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Throughput is measured over this trailing window.
pub(crate) const THROUGHPUT_WINDOW: Duration = Duration::from_secs(300);
/// Rates are not reported until this much of the window has elapsed, so a
/// single burst at start-up cannot produce a wild estimate.
const MIN_RATE_SPAN: Duration = Duration::from_secs(10);
/// Sparkline resolution and length.
pub(crate) const SPARKLINE_BUCKET: Duration = Duration::from_secs(30);
pub(crate) const SPARKLINE_BUCKETS: usize = 20;
const MAX_SAMPLES: usize = 4_096;
const MAX_TRACKED_JOBS: usize = 100_000;

#[derive(Clone, Debug)]
pub(crate) struct JobTelemetry {
    pub(crate) progress: TransferProgress,
    pub(crate) last_update: Instant,
    pub(crate) finished: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetryNote {
    pub(crate) attempt: u32,
    pub(crate) max_retries: u32,
    pub(crate) retry_at: Instant,
    pub(crate) failure_class: &'static str,
}

/// One rate domain paused after provider pushback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CooldownNote {
    pub(crate) label: String,
    pub(crate) until: Instant,
    /// Canonical provider; `global` pauses every provider.
    pub(crate) provider: &'static str,
    /// `host:port` of the paused domain; `None` for the global domain.
    pub(crate) endpoint: Option<String>,
    /// Adaptive concurrency ceiling after the observed capacity event.
    pub(crate) current_limit: usize,
    /// Configured organization/batch ceiling for this domain.
    pub(crate) configured_limit: usize,
    /// Consecutive capacity failures in the current escalation episode.
    pub(crate) consecutive_failures: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CooldownDetails {
    pub(crate) current_limit: usize,
    pub(crate) configured_limit: usize,
    pub(crate) consecutive_failures: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FailureNote {
    /// `None` for a single-mailbox run.
    pub(crate) job_id: Option<String>,
    pub(crate) state: String,
    pub(crate) detail: String,
    pub(crate) at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Throughput {
    pub(crate) bytes_per_second: f64,
    pub(crate) messages_per_second: f64,
}

/// Where an estimated finish lands relative to the maintenance window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowRisk {
    OnTrack,
    Tight,
    Overrun,
}

#[derive(Default)]
pub(crate) struct RunTelemetry {
    started_at: Option<Instant>,
    /// Mailboxes in this run's scope (1 for a single-mailbox run).
    scope: usize,
    jobs: HashMap<String, JobTelemetry>,
    /// (time, cumulative bytes, cumulative messages) across all jobs.
    samples: VecDeque<(Instant, u64, u64)>,
    retries: HashMap<String, RetryNote>,
    cooldowns: BTreeMap<String, CooldownNote>,
    completions: Vec<Instant>,
    latest_failure: Option<FailureNote>,
}

impl RunTelemetry {
    pub(crate) fn reset(&mut self, now: Instant, scope: usize) {
        *self = Self {
            started_at: Some(now),
            scope,
            ..Self::default()
        };
        self.samples.push_back((now, 0, 0));
    }

    pub(crate) fn started_at(&self) -> Option<Instant> {
        self.started_at
    }

    pub(crate) fn scope(&self) -> usize {
        self.scope
    }

    pub(crate) fn completed(&self) -> usize {
        self.completions.len()
    }

    pub(crate) fn record_progress(
        &mut self,
        job_id: &str,
        progress: TransferProgress,
        now: Instant,
    ) {
        if self.started_at.is_none() {
            self.reset(now, 1);
        }
        if self.jobs.len() >= MAX_TRACKED_JOBS && !self.jobs.contains_key(job_id) {
            return;
        }
        let entry = self.jobs.entry(job_id.to_owned()).or_insert(JobTelemetry {
            progress,
            last_update: now,
            finished: false,
        });
        // Snapshots are cumulative per process; a retry restarts the engine,
        // so keep the larger counters rather than moving backwards.
        if progress.bytes_copied >= entry.progress.bytes_copied
            || progress.messages_copied >= entry.progress.messages_copied
        {
            entry.progress = progress;
        }
        entry.last_update = now;
        self.retries.remove(job_id);
        let (bytes, messages) = self.totals();
        self.samples.push_back((now, bytes, messages));
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
    }

    pub(crate) fn record_retry(&mut self, job_id: &str, note: RetryNote) {
        self.retries.insert(job_id.to_owned(), note);
    }

    pub(crate) fn record_cooldown(
        &mut self,
        label: &str,
        provider: &'static str,
        endpoint: Option<String>,
        until: Instant,
        details: CooldownDetails,
    ) {
        let entry = self
            .cooldowns
            .entry(label.to_owned())
            .or_insert_with(|| CooldownNote {
                label: label.to_owned(),
                until,
                provider,
                endpoint,
                current_limit: details.current_limit,
                configured_limit: details.configured_limit,
                consecutive_failures: details.consecutive_failures,
            });
        entry.until = entry.until.max(until);
        entry.current_limit = details.current_limit;
        entry.configured_limit = details.configured_limit;
        entry.consecutive_failures = details.consecutive_failures;
    }

    pub(crate) fn record_job_finished(
        &mut self,
        job_id: &str,
        state: &str,
        detail: &str,
        now: Instant,
    ) {
        if let Some(job) = self.jobs.get_mut(job_id) {
            job.finished = true;
            job.last_update = now;
        }
        self.retries.remove(job_id);
        self.completions.push(now);
        if crate::ui::needs_operator_review(state) {
            self.latest_failure = Some(FailureNote {
                job_id: Some(job_id.to_owned()),
                state: state.to_owned(),
                detail: detail.to_owned(),
                at: now,
            });
        }
    }

    pub(crate) fn record_run_failure(&mut self, detail: &str, now: Instant) {
        self.latest_failure = Some(FailureNote {
            job_id: None,
            state: "failed".into(),
            detail: detail.to_owned(),
            at: now,
        });
    }

    pub(crate) fn totals(&self) -> (u64, u64) {
        self.jobs.values().fold((0, 0), |(bytes, messages), job| {
            (
                bytes.saturating_add(job.progress.bytes_copied),
                messages.saturating_add(job.progress.messages_copied),
            )
        })
    }

    /// Unfinished jobs with engine activity, most recently updated first.
    pub(crate) fn active_jobs(&self) -> Vec<(&str, &JobTelemetry)> {
        let mut jobs = self
            .jobs
            .iter()
            .filter(|(_, job)| !job.finished)
            .map(|(id, job)| (id.as_str(), job))
            .collect::<Vec<_>>();
        jobs.sort_by(|left, right| right.1.last_update.cmp(&left.1.last_update));
        jobs
    }

    pub(crate) fn job(&self, job_id: &str) -> Option<&JobTelemetry> {
        self.jobs.get(job_id)
    }

    /// Rolling throughput over `THROUGHPUT_WINDOW`, measured up to `now` so a
    /// stalled transfer decays toward zero instead of freezing its last rate.
    pub(crate) fn throughput(&self, now: Instant) -> Option<Throughput> {
        let window_start = now.checked_sub(THROUGHPUT_WINDOW).unwrap_or(now);
        let (base_time, base_bytes, base_messages) = self
            .samples
            .iter()
            .rev()
            .find(|(at, _, _)| *at <= window_start)
            .or_else(|| self.samples.front())
            .copied()?;
        let span = now.saturating_duration_since(base_time.max(window_start));
        if span < MIN_RATE_SPAN {
            return None;
        }
        let (bytes, messages) = self.totals();
        let seconds = span.as_secs_f64();
        Some(Throughput {
            bytes_per_second: bytes.saturating_sub(base_bytes) as f64 / seconds,
            messages_per_second: messages.saturating_sub(base_messages) as f64 / seconds,
        })
    }

    /// Bytes per second in consecutive `SPARKLINE_BUCKET`s ending at `now`,
    /// oldest first. Buckets before the run started are omitted.
    pub(crate) fn throughput_series(&self, now: Instant) -> Vec<f64> {
        let Some(started) = self.started_at else {
            return Vec::new();
        };
        let cumulative_at = |at: Instant| {
            self.samples
                .iter()
                .rev()
                .find(|(sample_at, _, _)| *sample_at <= at)
                .map_or(0, |(_, bytes, _)| *bytes)
        };
        (0..SPARKLINE_BUCKETS)
            .rev()
            .filter_map(|index| {
                let end = now.checked_sub(SPARKLINE_BUCKET * index as u32)?;
                let begin = end.checked_sub(SPARKLINE_BUCKET)?;
                (end > started).then(|| {
                    let begin = begin.max(started);
                    let seconds = end.saturating_duration_since(begin).as_secs_f64().max(1.0);
                    cumulative_at(end).saturating_sub(cumulative_at(begin)) as f64 / seconds
                })
            })
            .collect()
    }

    /// Estimated bytes still to move for mailboxes already in flight.
    pub(crate) fn in_flight_remaining_bytes(&self) -> Option<u64> {
        let mut any = false;
        let total = self
            .jobs
            .values()
            .filter(|job| !job.finished)
            .filter_map(|job| job.progress.estimated_remaining_bytes())
            .inspect(|_| any = true)
            .fold(0_u64, u64::saturating_add);
        any.then_some(total)
    }

    /// Estimated time to finish the mailboxes already in flight, from their
    /// remaining bytes (or imapsync's messages-left count when sizes are
    /// unknown) and the rolling throughput.
    pub(crate) fn in_flight_eta(&self, now: Instant) -> Option<Duration> {
        let rate = self.throughput(now)?;
        if let Some(remaining) = self.in_flight_remaining_bytes()
            && rate.bytes_per_second > 0.0
        {
            return Some(Duration::from_secs_f64(
                remaining as f64 / rate.bytes_per_second,
            ));
        }
        let left = self
            .jobs
            .values()
            .filter(|job| !job.finished)
            .filter_map(|job| job.progress.messages_left)
            .fold(0_u64, u64::saturating_add);
        (left > 0 && rate.messages_per_second > 0.0)
            .then(|| Duration::from_secs_f64(left as f64 / rate.messages_per_second))
    }

    /// Estimated time to finish the whole queue. While mailboxes are still
    /// waiting, extrapolate from the observed mailbox completion rate (needs
    /// at least two completions); otherwise use the in-flight estimate.
    pub(crate) fn eta(&self, waiting_mailboxes: usize, now: Instant) -> Option<Duration> {
        if waiting_mailboxes == 0 {
            return self.in_flight_eta(now);
        }
        let started = self.started_at?;
        if self.completions.len() < 2 {
            return None;
        }
        let elapsed = now.saturating_duration_since(started).as_secs_f64();
        let per_mailbox = elapsed / self.completions.len() as f64;
        let queue = Duration::from_secs_f64(per_mailbox * waiting_mailboxes as f64);
        Some(queue.max(self.in_flight_eta(now).unwrap_or_default()))
    }

    pub(crate) fn pending_retries(&self, now: Instant) -> Vec<(&str, &RetryNote)> {
        let mut retries = self
            .retries
            .iter()
            .filter(|(_, note)| note.retry_at > now)
            .map(|(id, note)| (id.as_str(), note))
            .collect::<Vec<_>>();
        retries.sort_by_key(|(_, note)| note.retry_at);
        retries
    }

    pub(crate) fn active_cooldowns(&self, now: Instant) -> Vec<(&str, Duration)> {
        self.cooldowns
            .iter()
            .filter(|(_, note)| note.until > now)
            .map(|(label, note)| (label.as_str(), note.until.saturating_duration_since(now)))
            .collect()
    }

    /// Paused domains still counting down, for per-provider health.
    pub(crate) fn active_cooldown_notes(&self, now: Instant) -> Vec<&CooldownNote> {
        self.cooldowns
            .values()
            .filter(|note| note.until > now)
            .collect()
    }

    pub(crate) fn latest_failure(&self) -> Option<&FailureNote> {
        self.latest_failure.as_ref()
    }

    /// Whether anything time-based is still counting down, so the view keeps
    /// repainting while idle input would otherwise freeze the countdowns.
    pub(crate) fn has_live_countdowns(&self, now: Instant) -> bool {
        self.retries.values().any(|note| note.retry_at > now)
            || self.cooldowns.values().any(|note| note.until > now)
    }
}

pub(crate) fn window_risk(eta: Duration, closes_in: Duration) -> WindowRisk {
    if eta.as_secs_f64() <= closes_in.as_secs_f64() * 0.8 {
        WindowRisk::OnTrack
    } else if eta <= closes_in {
        WindowRisk::Tight
    } else {
        WindowRisk::Overrun
    }
}

#[cfg(test)]
mod tests {
    use super::{CooldownDetails, RetryNote, RunTelemetry, WindowRisk, window_risk};
    use crate::progress::TransferProgress;
    use std::time::{Duration, Instant};

    fn progress(bytes: u64, messages: u64, source_bytes: Option<u64>) -> TransferProgress {
        TransferProgress {
            bytes_copied: bytes,
            messages_copied: messages,
            source_bytes,
            ..TransferProgress::default()
        }
    }

    #[test]
    fn rolling_throughput_and_in_flight_eta_follow_remaining_bytes() {
        let start = Instant::now();
        let mut telemetry = RunTelemetry::default();
        telemetry.reset(start, 1);
        telemetry.record_progress("a", progress(0, 0, Some(1_000_000)), start);
        let now = start + Duration::from_secs(100);
        telemetry.record_progress("a", progress(400_000, 40, Some(1_000_000)), now);
        let rate = telemetry.throughput(now).unwrap();
        assert!((rate.bytes_per_second - 4_000.0).abs() < 1.0);
        assert!((rate.messages_per_second - 0.4).abs() < 0.01);
        let eta = telemetry.in_flight_eta(now).unwrap();
        assert_eq!(eta.as_secs(), 150);
        // A stall decays the rate rather than freezing it.
        let later = now + Duration::from_secs(100);
        assert!(telemetry.throughput(later).unwrap().bytes_per_second < 2_100.0);
    }

    #[test]
    fn no_rate_is_reported_before_the_minimum_span() {
        let start = Instant::now();
        let mut telemetry = RunTelemetry::default();
        telemetry.reset(start, 1);
        telemetry.record_progress(
            "a",
            progress(10_000, 1, None),
            start + Duration::from_secs(2),
        );
        assert!(
            telemetry
                .throughput(start + Duration::from_secs(3))
                .is_none()
        );
        assert!(
            telemetry
                .in_flight_eta(start + Duration::from_secs(3))
                .is_none()
        );
    }

    #[test]
    fn queue_eta_needs_two_completions_and_scales_with_waiting_mailboxes() {
        let start = Instant::now();
        let mut telemetry = RunTelemetry::default();
        telemetry.reset(start, 1);
        telemetry.record_job_finished("a", "verified", "", start + Duration::from_secs(60));
        assert!(telemetry.eta(4, start + Duration::from_secs(60)).is_none());
        telemetry.record_job_finished("b", "verified", "", start + Duration::from_secs(120));
        let eta = telemetry.eta(4, start + Duration::from_secs(120)).unwrap();
        assert_eq!(eta.as_secs(), 240);
    }

    #[test]
    fn retries_cooldowns_and_failures_are_tracked_until_resolved() {
        let start = Instant::now();
        let mut telemetry = RunTelemetry::default();
        telemetry.reset(start, 1);
        telemetry.record_retry(
            "a",
            RetryNote {
                attempt: 2,
                max_retries: 3,
                retry_at: start + Duration::from_secs(30),
                failure_class: "capacity",
            },
        );
        telemetry.record_cooldown(
            "source provider src:993",
            "generic",
            Some("src:993".into()),
            start + Duration::from_secs(60),
            CooldownDetails {
                current_limit: 2,
                configured_limit: 8,
                consecutive_failures: 1,
            },
        );
        assert_eq!(telemetry.pending_retries(start).len(), 1);
        assert_eq!(telemetry.active_cooldowns(start)[0].1.as_secs(), 60);
        let note = telemetry.active_cooldown_notes(start).remove(0);
        assert_eq!(note.current_limit, 2);
        assert_eq!(note.configured_limit, 8);
        assert_eq!(note.consecutive_failures, 1);
        assert!(telemetry.has_live_countdowns(start));
        assert!(!telemetry.has_live_countdowns(start + Duration::from_secs(61)));
        telemetry.record_job_finished("a", "failed", "auth rejected", start);
        assert!(telemetry.pending_retries(start).is_empty());
        let failure = telemetry.latest_failure().unwrap();
        assert_eq!(failure.job_id.as_deref(), Some("a"));
        assert_eq!(failure.detail, "auth rejected");
        telemetry.record_job_finished("b", "verified", "", start);
        assert_eq!(
            telemetry.latest_failure().unwrap().job_id.as_deref(),
            Some("a")
        );
    }

    #[test]
    fn sparkline_buckets_report_bytes_per_second() {
        let start = Instant::now();
        let mut telemetry = RunTelemetry::default();
        telemetry.reset(start, 1);
        telemetry.record_progress(
            "a",
            progress(30_000, 3, None),
            start + Duration::from_secs(30),
        );
        telemetry.record_progress(
            "a",
            progress(90_000, 9, None),
            start + Duration::from_secs(60),
        );
        let series = telemetry.throughput_series(start + Duration::from_secs(60));
        assert_eq!(series.len(), 2);
        assert!((series[0] - 1_000.0).abs() < 1.0);
        assert!((series[1] - 2_000.0).abs() < 1.0);
    }

    #[test]
    fn window_risk_leaves_a_safety_margin() {
        let window = Duration::from_secs(100);
        assert_eq!(
            window_risk(Duration::from_secs(80), window),
            WindowRisk::OnTrack
        );
        assert_eq!(
            window_risk(Duration::from_secs(95), window),
            WindowRisk::Tight
        );
        assert_eq!(
            window_risk(Duration::from_secs(101), window),
            WindowRisk::Overrun
        );
    }
}
