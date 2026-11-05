//! Persistent account usage in the composer's bottom hint row.

use crate::clock_format::ClockFormat;
use crate::footer_hint::first_fitting_line;
use chrono::DateTime;
use chrono::Local;
use ratatui::style::Style;
use ratatui::text::Line;

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct UsageIndicator {
    pub(crate) used_percent: i32,
    pub(crate) resets_at: Option<i64>,
    pub(crate) unlimited: bool,
    pub(crate) limit_reached: bool,
    pub(crate) clock_format: ClockFormat,
}

impl UsageIndicator {
    pub(crate) fn line(self, width: u16, now: DateTime<Local>) -> Option<Line<'static>> {
        let line = first_fitting_line(self.labels(now), width);
        (!line.spans.is_empty()).then_some(line.style(self.style()))
    }

    /// Longest to shortest forms, matching the warning label collapse order.
    pub(crate) fn labels(self, now: DateTime<Local>) -> [Line<'static>; 3] {
        let remaining = 100 - self.used_percent.clamp(0, 100);
        let summary = if self.limit_reached {
            "Usage limit reached".to_string()
        } else if self.unlimited {
            "Usage unlimited".to_string()
        } else {
            format!("Usage {remaining}% left")
        };
        let reset = self
            .resets_at
            .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0))
            .filter(|reset| *reset > now)
            .map(|reset| {
                format!(
                    " · resets {}",
                    crate::status::format_reset_timestamp(
                        reset.with_timezone(&Local),
                        now,
                        self.clock_format,
                    )
                )
            })
            .unwrap_or_default();
        let compact = if self.limit_reached {
            "Limit reached".to_string()
        } else if self.unlimited {
            "Unlimited".to_string()
        } else {
            format!("Usage {remaining}%")
        };
        [
            Line::from(format!("{summary}{reset}")),
            Line::from(summary),
            Line::from(compact),
        ]
    }

    fn style(self) -> Style {
        if self.limit_reached {
            Style::default().red()
        } else {
            Style::default().cyan()
        }
    }
}
