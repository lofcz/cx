//! Account quota state projects to the persistent bottom usage indicator.
//! Rolling updates retain metadata; fresh account reads replace windows and confirm recovery.
//! Sparse updates preserve quota metadata and account reads confirm recovery.

use super::ChatWidget;
use super::rate_limits::RateLimitSnapshotSource;
use super::rate_limits::is_approximate_window;
use crate::clock_format::ClockFormat;
#[cfg(test)]
use chrono::DateTime;
#[cfg(test)]
use chrono::Local;
use codex_app_server_protocol::CreditsSnapshot;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_app_server_protocol::RateLimitWindow;
use codex_protocol::account::PlanType;
#[cfg(test)]
use ratatui::text::Line;

#[derive(Default)]
pub(super) struct UsageNoticeState {
    primary: Option<RateLimitWindow>,
    secondary: Option<RateLimitWindow>,
    plan_type: Option<PlanType>,
    credits: Option<CreditsSnapshot>,
    latest_read_id: u64,
    stale_through: Option<u64>,
    applying_stale_read: bool,
}

impl UsageNoticeState {
    pub(super) fn indicator(
        &self,
        clock_format: ClockFormat,
        limit_reached: bool,
    ) -> Option<crate::bottom_pane::UsageIndicator> {
        let window = [self.secondary.as_ref(), self.primary.as_ref()]
            .into_iter()
            .flatten()
            .max_by_key(|window| window.used_percent);
        let unlimited = self
            .credits
            .as_ref()
            .is_some_and(|credits| credits.unlimited);
        if window.is_none() && !unlimited && !limit_reached {
            return None;
        }
        Some(crate::bottom_pane::UsageIndicator {
            used_percent: window.map_or(0, |window| window.used_percent),
            resets_at: window.and_then(|window| window.resets_at),
            unlimited,
            limit_reached,
            clock_format,
        })
    }

    #[cfg(test)]
    fn line(
        &self,
        width: u16,
        now: DateTime<Local>,
        clock_format: ClockFormat,
    ) -> Option<Line<'static>> {
        self.indicator(clock_format, false)?.line(width, now)
    }

    pub(super) fn update(
        &mut self,
        snapshot: &RateLimitSnapshot,
        source: RateLimitSnapshotSource,
        plan_type: Option<PlanType>,
    ) -> bool {
        if matches!(source, RateLimitSnapshotSource::AccountUsage) && self.applying_stale_read {
            return false;
        }
        let previous = self.indicator(ClockFormat::TwentyFourHour, false);
        let rolling = matches!(source, RateLimitSnapshotSource::RollingUpdate);
        if rolling {
            self.stale_through = Some(self.latest_read_id);
        }
        self.plan_type = snapshot.plan_type.or(self.plan_type).or(plan_type);
        for (current, incoming) in [
            (&mut self.primary, &snapshot.primary),
            (&mut self.secondary, &snapshot.secondary),
        ] {
            if !rolling {
                *current = incoming.clone();
                continue;
            }
            let Some(incoming) = incoming else {
                continue;
            };
            let mut window = incoming.clone();
            if let Some(previous) = current {
                window.window_duration_mins = window
                    .window_duration_mins
                    .or(previous.window_duration_mins);
                window.resets_at = window.resets_at.or(previous.resets_at);
                let threshold = warning_threshold(self.plan_type, window.window_duration_mins);
                if previous.used_percent >= threshold && window.used_percent < threshold {
                    continue;
                }
            }
            *current = Some(window);
        }
        if !rolling || snapshot.credits.is_some() {
            self.credits = snapshot.credits.clone();
        }
        self.indicator(ClockFormat::TwentyFourHour, false) != previous
    }

    /// Select the lowest remaining capacity and whether it is the secondary window.
    #[cfg(test)]
    pub(super) fn current(&self) -> Option<(RateLimitWindow, bool)> {
        if self
            .credits
            .as_ref()
            .is_some_and(|credits| credits.unlimited || credits.has_credits)
        {
            return None;
        }
        // max_by_key chooses the last equal entry, giving the primary window priority.
        [(&self.secondary, true), (&self.primary, false)]
            .into_iter()
            .filter_map(|(window, secondary)| Some((window.as_ref()?, secondary)))
            .filter(|(window, _)| {
                window.used_percent
                    >= warning_threshold(self.plan_type, window.window_duration_mins)
            })
            .max_by_key(|(window, _)| window.used_percent)
            .map(|(window, secondary)| (window.clone(), secondary))
    }
}

pub(super) fn warning_threshold(plan_type: Option<PlanType>, window_minutes: Option<i64>) -> i32 {
    if matches!(plan_type, Some(PlanType::Plus | PlanType::Team))
        && window_minutes.is_some_and(|minutes| {
            is_approximate_window(minutes, /*expected_minutes*/ 5 * 60)
        })
    {
        50
    } else {
        75
    }
}

impl ChatWidget {
    #[cfg(test)]
    pub(crate) fn usage_notice(&self, width: u16) -> Option<Line<'static>> {
        self.usage_notice_state
            .indicator(
                self.clock_format,
                self.input_queue.quota_exhausted
                    || self.codex_rate_limit_reached_type.is_some()
                    || self.codex_spend_control_reached == Some(true),
            )?
            .line(width, Local::now())
    }

    pub(crate) fn start_usage_notice_read(&mut self, request_id: u64) {
        self.usage_notice_state.latest_read_id = request_id;
    }

    pub(crate) fn apply_usage_notice_read(&mut self, request_id: u64) {
        // A newer failed read does not invalidate an older successful one. Only rolling
        // updates received since that read began make it too old to confirm recovery.
        self.usage_notice_state.applying_stale_read = self
            .usage_notice_state
            .stale_through
            .is_some_and(|id| request_id <= id);
    }
}

#[cfg(test)]
#[path = "usage_notice_tests.rs"]
mod tests;
