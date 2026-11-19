//! Persistent account usage in the composer's bottom hint row.

use crate::clock_format::ClockFormat;
use crate::footer_hint::first_fitting_line;
use chrono::DateTime;
use chrono::Local;
use ratatui::style::Style;
use ratatui::text::Line;

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct UsageIndicator {
    pub(crate) account: Option<&'static str>,
    pub(crate) reset_credits: Option<i64>,
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
    pub(crate) fn labels(self, now: DateTime<Local>) -> [Line<'static>; 4] {
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
        let named = |label: String| {
            Line::from(match self.account {
                Some(name) => format!("{name} · {label}"),
                None => label,
            })
        };
        let credits = match self.reset_credits {
            Some(1) => " · 1 reset credit".to_string(),
            Some(count) if count > 1 => format!(" · {count} reset credits"),
            _ => String::new(),
        };
        [
            named(format!("{summary}{reset}{credits}")),
            named(summary),
            named(compact.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_footer_keeps_quota_when_space_is_tight() {
        let usage = UsageIndicator {
            account: Some("factory"),
            reset_credits: Some(2),
            used_percent: 81,
            resets_at: None,
            unlimited: false,
            limit_reached: false,
            clock_format: ClockFormat::TwentyFourHour,
        };
        let now = Local::now();
        assert_eq!(
            usage.line(80, now).unwrap().to_string(),
            "factory · Usage 19% left · 2 reset credits"
        );
        assert_eq!(
            usage.line(19, now).unwrap().to_string(),
            "factory · Usage 19%"
        );
        assert_eq!(usage.line(9, now).unwrap().to_string(), "Usage 19%");
    }
}
