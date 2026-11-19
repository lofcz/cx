//! Draft-owned push-to-talk dictation. Only release (or Enter) hands audio to Soniox.
mod capture;
mod soniox;

use super::{ChatWidget, CustomPromptView};
use crate::app_event::AppEvent;
use crate::tui::TuiEvent;
use anyhow::{Context, Result};
use capture::Capture;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Some terminals emit Release followed by Repeat (or another modified Press)
/// for each keyboard repeat. Commit the release only after the pair can arrive.
#[derive(Default)]
pub(super) struct DictationHold {
    released_at: Option<Instant>,
}

impl DictationHold {
    const RELEASE_GRACE: Duration = Duration::from_millis(90);

    fn observe(&mut self, key: KeyEvent, now: Instant) {
        if key.code != KeyCode::Char('w') {
            return;
        }
        match key.kind {
            KeyEventKind::Release => {
                self.released_at.get_or_insert(now);
            }
            KeyEventKind::Repeat => self.released_at = None,
            KeyEventKind::Press if key.modifiers.contains(KeyModifiers::ALT) => {
                self.released_at = None
            }
            _ => {}
        }
    }

    fn released(&self, now: Instant) -> bool {
        self.released_at
            .is_some_and(|at| now.duration_since(at) >= Self::RELEASE_GRACE)
    }
}

pub(super) enum Dictation {
    Recording {
        capture: Capture,
        hold: DictationHold,
        start: Instant,
    },
    Processing {
        result: oneshot::Receiver<Result<String>>,
        start: Instant,
    },
}

impl Dictation {
    pub(super) fn panel(&self, animate: bool) -> Paragraph<'static> {
        let (label, hint, accent, indicator, elapsed) = match self {
            Self::Recording { capture, start, .. } => {
                let level = (capture.level.load(Ordering::Relaxed) as f32 / 32768.0).sqrt();
                let bars = (level * 12.0).round() as usize;
                (
                    "Recording",
                    "Release W or press Enter to transcribe · Esc cancels",
                    Color::LightRed,
                    format!("{}{}", "▰".repeat(bars), "▱".repeat(12 - bars)),
                    start.elapsed(),
                )
            }
            Self::Processing { start, .. } => {
                let elapsed = start.elapsed();
                let phase = if animate {
                    (elapsed.as_millis() / 160) as usize % 6
                } else {
                    0
                };
                (
                    "Transcribing",
                    "Soniox · Esc dismisses",
                    Color::LightCyan,
                    format!("{}◆{}", "· ".repeat(phase), " ·".repeat(5 - phase)),
                    elapsed,
                )
            }
        };
        let seconds = elapsed.as_secs();
        Paragraph::new(vec![
            Line::from(vec![
                "  ".into(),
                label.fg(accent).bold(),
                format!("   {indicator}").fg(accent),
                format!("   {}:{:02}", seconds / 60, seconds % 60).dim(),
            ]),
            Line::from(format!("  {hint}").dim()),
        ])
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(accent),
        )
    }
}

impl ChatWidget {
    fn dictation_model(&self) -> &str {
        self.local_settings
            .tui
            .dictation_model
            .as_deref()
            .unwrap_or("stt-async-v6")
    }

    fn soniox_key(&self) -> Result<String> {
        let key = std::env::var("SONIOX_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                std::fs::read_to_string(self.local_settings.codex_home.join("soniox.key")).ok()
            })
            .unwrap_or_default();
        anyhow::ensure!(
            !key.trim().is_empty(),
            "Set SONIOX_API_KEY or put the key in your Codex home’s soniox.key file."
        );
        Ok(key.trim().to_owned())
    }

    pub(super) fn open_dictation_settings(&mut self) {
        let tx = self.app_event_tx.clone();
        let instructions = if self.soniox_key().is_ok() {
            "API key ready · Hold Alt+W to dictate"
        } else {
            "Set SONIOX_API_KEY or save it in $CODEX_HOME/soniox.key"
        };
        self.bottom_pane.show_text_prompt(CustomPromptView::new(
            "Voice dictation · Soniox model".into(),
            "Asynchronous transcription model".into(),
            self.dictation_model().into(),
            Some(instructions.into()),
            Box::new(move |model| tx.send(AppEvent::SaveDictationModel(model))),
        ));
    }

    pub(crate) fn handle_dictation_event(&mut self, event: &TuiEvent, available: bool) -> bool {
        if self.dictation.is_none() {
            if available
                && !self.is_read_only_view()
                && !self.fork_in_progress
                && self.provider_error.is_none()
                && self.bottom_pane.composer_input_enabled()
                && matches!(event, TuiEvent::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('w') && key.modifiers == KeyModifiers::ALT)
            {
                match self.soniox_key().and_then(|_| Capture::start()) {
                    Ok(capture) => {
                        self.dictation = Some(Dictation::Recording {
                            capture,
                            hold: DictationHold::default(),
                            start: Instant::now(),
                        })
                    }
                    Err(err) => self.add_error_message(format!("{err:#}")),
                }
                self.frame_requester.schedule_frame();
                return true;
            }
            return false;
        }

        if !available || matches!(event, TuiEvent::FocusLost | TuiEvent::Resume) {
            if matches!(self.dictation, Some(Dictation::Recording { .. })) {
                self.dictation = None;
                self.frame_requester.schedule_frame();
            }
            return false;
        }
        match event {
            TuiEvent::Key(key) => {
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc {
                    self.dictation = None;
                } else if let Some(Dictation::Recording { hold, .. }) = self.dictation.as_mut() {
                    if key.code == KeyCode::Enter && key.kind == KeyEventKind::Press {
                        self.finish_dictation();
                    } else {
                        hold.observe(*key, Instant::now());
                        if hold.released_at.is_some() {
                            self.frame_requester
                                .schedule_frame_in(DictationHold::RELEASE_GRACE);
                        }
                    }
                }
                self.frame_requester.schedule_frame();
                true
            }
            TuiEvent::Mouse(_) | TuiEvent::Paste(_) => true,
            TuiEvent::Draw => {
                if let Some(Dictation::Recording { hold, .. }) = &self.dictation
                    && hold.released(Instant::now())
                {
                    self.finish_dictation();
                }
                let outcome = match self.dictation.as_mut() {
                    Some(Dictation::Recording { capture, start, .. })
                        if capture.ended() || start.elapsed().as_secs() >= capture::MAX_SECONDS =>
                    {
                        Some(Err(anyhow::anyhow!(
                            "Microphone recording stopped. Check your input device and keep dictations under five minutes."
                        )))
                    }
                    Some(Dictation::Processing { result, .. }) => match result.try_recv() {
                        Ok(result) => Some(result),
                        Err(oneshot::error::TryRecvError::Empty) => None,
                        Err(oneshot::error::TryRecvError::Closed) => {
                            Some(Err(anyhow::anyhow!("Transcription worker stopped.")))
                        }
                    },
                    _ => None,
                };
                if let Some(outcome) = outcome {
                    self.dictation = None;
                    match outcome {
                        Ok(text) => self.bottom_pane.insert_str(&text),
                        Err(err) => self.add_error_message(format!("{err:#}")),
                    }
                } else {
                    self.frame_requester
                        .schedule_frame_in(Duration::from_millis(100));
                }
                false
            }
            _ => false,
        }
    }

    fn finish_dictation(&mut self) {
        let Some(Dictation::Recording { capture, .. }) = self.dictation.take() else {
            return;
        };
        let input = capture
            .finish()
            .context("Could not finish recording")
            .and_then(|wav| Ok((wav, self.soniox_key()?)));
        match input {
            Ok((wav, key)) => {
                let factory = self.config.http_client_factory();
                let model = self.dictation_model().to_owned();
                let (tx, result) = oneshot::channel();
                tokio::spawn(async move {
                    let _ = tx.send(soniox::transcribe(factory, wav, model, key).await);
                });
                self.dictation = Some(Dictation::Processing {
                    result,
                    start: Instant::now(),
                });
            }
            Err(err) => self.add_error_message(format!("{err:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
    #[test]
    fn sustained_hold_survives_repeat_release_pairs_until_key_up() {
        let mut hold = DictationHold::default();
        let mut now = Instant::now();
        let w = |kind, modifiers| KeyEvent::new_with_kind(KeyCode::Char('w'), modifiers, kind);
        hold.observe(w(KeyEventKind::Press, KeyModifiers::ALT), now);
        now += Duration::from_millis(500);
        for n in 0..120 {
            hold.observe(w(KeyEventKind::Release, KeyModifiers::ALT), now);
            assert!(!hold.released(now));
            now += Duration::from_millis(2);
            let down = if n % 2 == 0 {
                KeyEventKind::Repeat
            } else {
                KeyEventKind::Press
            };
            hold.observe(w(down, KeyModifiers::ALT), now);
            now += Duration::from_millis(30);
            assert!(!hold.released(now));
        }
        // Releasing Alt first may leave the physical W release unmodified.
        hold.observe(w(KeyEventKind::Release, KeyModifiers::NONE), now);
        assert!(hold.released(now + DictationHold::RELEASE_GRACE));
    }

    #[tokio::test]
    async fn processing_result_belongs_to_the_open_draft() {
        let (mut widget, _, _, _) = make_chatwidget_manual_with_sender().await;
        widget.bottom_pane.insert_str("before ");
        let (tx, result) = oneshot::channel();
        widget.dictation = Some(Dictation::Processing {
            result,
            start: Instant::now(),
        });
        assert!(widget.handle_dictation_event(&TuiEvent::Paste("ignored".into()), true));
        tx.send(Ok("spoken words".into())).unwrap();
        widget.handle_dictation_event(&TuiEvent::Draw, false);
        assert_eq!(widget.bottom_pane.composer_text(), "before ");
        widget.handle_dictation_event(&TuiEvent::Draw, true);
        assert_eq!(widget.bottom_pane.composer_text(), "before spoken words");
        assert!(widget.dictation.is_none());

        let (tx, result) = oneshot::channel();
        widget.dictation = Some(Dictation::Processing {
            result,
            start: Instant::now(),
        });
        let escape = TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(widget.handle_dictation_event(&escape, true));
        assert!(tx.send(Ok("discarded".into())).is_err());
        assert_eq!(widget.bottom_pane.composer_text(), "before spoken words");
    }
}
