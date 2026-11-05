use codex_protocol::ThreadId;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;

use super::PickerLoadRequest;
use super::PickerState;
use super::SessionSelection;
use super::SessionStatus;
use super::archive::ArchiveState;
use crate::backend_id::BackendId;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::KeymapContext;

/// Inline rename session inside the picker.
///
/// `Editing` owns the draft until the user accepts or cancels; `Saving` is the
/// in-flight `thread/name/set` request, matched by thread id and name so a
/// stale response cannot clobber a newer edit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) enum RenameState {
    #[default]
    Idle,
    Editing {
        backend_id: BackendId,
        thread_id: ThreadId,
        draft: String,
    },
    Saving {
        backend_id: BackendId,
        thread_id: ThreadId,
        name: String,
    },
}

fn normalize_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

impl PickerState {
    /// Whether Ctrl+R may claim the rename shortcut. Archived threads stay
    /// read-only, and a user keymap that already binds Ctrl+R keeps precedence.
    pub(super) fn rename_shortcut_available(&self) -> bool {
        if self.status == SessionStatus::Archived {
            return false;
        }
        let rename_key = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        self.keymap.list.action_for(rename_key).is_none()
            && !self.keymap.chords.bindings.iter().any(|binding| {
                binding.action.context == KeymapContext::List
                    && binding.chord.prefix.is_press(rename_key)
            })
    }

    /// Begin an inline rename for the selected row.
    pub(super) fn begin_rename_selected(&mut self) {
        if !matches!(self.rename_state, RenameState::Idle)
            || !matches!(self.archive_state, ArchiveState::Idle)
        {
            return;
        }
        let Some(row) = self.filtered_rows.get(self.selected) else {
            return;
        };
        let Some(thread_id) = row.thread_id else {
            self.inline_error = Some(String::from("Selected session cannot be renamed."));
            self.request_frame();
            return;
        };
        self.rename_state = RenameState::Editing {
            backend_id: row.backend_id.unwrap_or(self.backend_id),
            thread_id,
            draft: String::new(),
        };
        self.request_frame();
    }

    /// Route a key while a rename edit is active. Owns all input except the
    /// global Ctrl+C exit; returns `Some` only when the picker should exit.
    pub(super) fn handle_rename_key(&mut self, key: KeyEvent) -> Option<SessionSelection> {
        if matches!(key,
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL))
        {
            return Some(SessionSelection::Exit);
        }

        match std::mem::take(&mut self.rename_state) {
            RenameState::Editing {
                backend_id,
                thread_id,
                mut draft,
            } => {
                if self.keymap.list.cancel.is_pressed(key) {
                    self.rename_state = RenameState::Idle;
                } else if self.keymap.list.accept.is_pressed(key) {
                    match normalize_name(&draft) {
                        Some(name) => {
                            self.rename_state = RenameState::Saving {
                                backend_id,
                                thread_id,
                                name: name.clone(),
                            };
                            (self.picker_loader)(PickerLoadRequest::Rename {
                                backend_id,
                                thread_id,
                                name,
                            });
                        }
                        None => {
                            self.rename_state = RenameState::Editing {
                                backend_id,
                                thread_id,
                                draft,
                            };
                            self.inline_error = Some(String::from("Thread name cannot be empty."));
                        }
                    }
                } else {
                    match key {
                        KeyEvent {
                            code: KeyCode::Backspace,
                            ..
                        } => {
                            draft.pop();
                        }
                        KeyEvent {
                            code: KeyCode::Char(c),
                            modifiers,
                            ..
                        } if !modifiers.contains(KeyModifiers::CONTROL)
                            && !modifiers.contains(KeyModifiers::ALT) =>
                        {
                            draft.push(c);
                        }
                        _ => {}
                    }
                    self.rename_state = RenameState::Editing {
                        backend_id,
                        thread_id,
                        draft,
                    };
                }
            }
            RenameState::Saving {
                backend_id,
                thread_id,
                name,
            } => {
                self.rename_state = if self.keymap.list.cancel.is_pressed(key) {
                    RenameState::Idle
                } else {
                    RenameState::Saving {
                        backend_id,
                        thread_id,
                        name,
                    }
                };
            }
            RenameState::Idle => {}
        }
        self.request_frame();
        None
    }

    /// Append pasted text to the rename draft when editing.
    pub(super) fn handle_rename_paste(&mut self, pasted: &str) -> bool {
        let RenameState::Editing { draft, .. } = &mut self.rename_state else {
            return false;
        };
        let Some(pasted) = crate::clipboard_paste::normalize_pasted_search_query(pasted) else {
            return true;
        };
        if !draft.is_empty() && !draft.ends_with(char::is_whitespace) {
            draft.push(' ');
        }
        draft.push_str(&pasted);
        self.request_frame();
        true
    }

    /// Apply a `thread/name/set` result: keep the row in sync with the server
    /// and reopen the editor on failure so the user can retry.
    pub(super) fn handle_rename_result(
        &mut self,
        backend_id: BackendId,
        thread_id: ThreadId,
        name: String,
        result: std::io::Result<()>,
    ) {
        let was_pending = matches!(
            self.rename_state,
            RenameState::Saving {
                backend_id: pending_backend,
                thread_id: pending_id,
                name: ref pending_name,
                ..
            } if pending_backend == backend_id && pending_id == thread_id && *pending_name == name
        );
        match result {
            Ok(()) => {
                if was_pending {
                    self.inline_error = None;
                }
                // Apply even when the user cancelled the wait: the name did
                // change server-side, so the row should reflect it.
                for row in self.all_rows.iter_mut() {
                    if row.thread_id == Some(thread_id)
                        && row.backend_id.unwrap_or(self.backend_id) == backend_id
                    {
                        row.thread_name = Some(name.clone());
                    }
                }
                if was_pending {
                    self.rename_state = RenameState::Idle;
                }
                self.apply_filter();
                if was_pending
                    && let Some(selected) = self.filtered_rows.iter().position(|row| {
                        row.thread_id == Some(thread_id)
                            && row.backend_id.unwrap_or(self.backend_id) == backend_id
                    })
                {
                    self.selected = selected;
                    self.ensure_selected_visible();
                }
            }
            Err(error) => {
                if !was_pending {
                    return;
                }
                self.rename_state = RenameState::Editing {
                    backend_id,
                    thread_id,
                    draft: name,
                };
                self.inline_error = Some(format!("Failed to rename session: {error}"));
            }
        }
        self.request_frame();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resume_picker::tests::{local_db_first_state, make_row};

    #[test]
    fn codext_rename_cancel_and_failed_save_preserve_draft() {
        let (mut state, _) = local_db_first_state();
        let thread_id = ThreadId::new();
        let mut row = make_row("session.jsonl", "2026-11-05T12:00:00Z", "preview");
        row.thread_id = Some(thread_id);
        state.all_rows = vec![row];
        state.apply_filter();
        state.begin_rename_selected();
        state.handle_rename_paste("new name");
        state.handle_rename_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(state.rename_state, RenameState::Idle);
        assert_eq!(state.all_rows[0].thread_name, None);
        state.rename_state = RenameState::Saving {
            backend_id: state.backend_id,
            thread_id,
            name: "new name".into(),
        };
        state.handle_rename_result(
            state.backend_id,
            thread_id,
            "new name".into(),
            Err(std::io::Error::other("offline")),
        );
        assert!(
            matches!(state.rename_state, RenameState::Editing { ref draft, .. } if draft == "new name")
        );
        state.rename_state = RenameState::Saving {
            backend_id: state.backend_id,
            thread_id,
            name: "new name".into(),
        };
        state.handle_rename_result(state.backend_id, thread_id, "new name".into(), Ok(()));
        assert_eq!(state.rename_state, RenameState::Idle);
        assert_eq!(state.all_rows[0].thread_name.as_deref(), Some("new name"));
    }

    #[test]
    fn codext_rename_result_stays_with_its_backend() {
        let (mut state, _) = local_db_first_state();
        let thread_id = ThreadId::new();
        let other_backend = BackendId::new();
        let mut local = make_row("local.jsonl", "2026-11-05T12:00:00Z", "local");
        local.thread_id = Some(thread_id);
        let mut remote = local.clone();
        remote.backend_id = Some(other_backend);
        state.all_rows = vec![local, remote];
        state.handle_rename_result(other_backend, thread_id, "remote name".into(), Ok(()));
        assert_eq!(state.all_rows[0].thread_name, None);
        assert_eq!(
            state.all_rows[1].thread_name.as_deref(),
            Some("remote name")
        );
        state.handle_rename_result(
            other_backend,
            thread_id,
            "old name".into(),
            Err(std::io::Error::other("stale")),
        );
        assert!(state.inline_error.is_none());
    }
}
