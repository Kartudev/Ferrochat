//! Drag and drop files onto the conversation to send them, like WhatsApp
//! Desktop: while files hover the window the open chat shows a "drop here"
//! overlay, and dropped files open the same attachment preview as the
//! paperclip (photos, videos, GIFs, documents).
//!
//! The window reports one `FileDropped` per file, back to back, so drops are
//! gathered for [`GATHER`] and staged together. Drag and drop comes from
//! winit, which supports it on X11 (and XWayland), Windows and macOS; native
//! Wayland windows get no drop events from winit 0.30.

use std::path::PathBuf;
use std::time::Duration;

use iced::Task;

use super::message::{Done, Message, Ui};
use super::{App, paste};

/// How long the files of one drop are gathered before staging them.
pub const GATHER: Duration = Duration::from_millis(80);

/// A drag-and-drop event of the window.
#[derive(Debug, Clone)]
pub enum DropMsg {
    /// Files are being dragged over the window.
    Hovered,
    /// The drag left the window or was cancelled.
    Left,
    /// One dropped file.
    Dropped(PathBuf),
    /// The files of one drop are all in.
    Gathered,
}

/// What is being dragged / dropped right now. Pure; driven by [`App`].
#[derive(Debug, Default)]
pub struct DropState {
    /// Files hover the window: the overlay is shown.
    pub hovering: bool,
    /// Files dropped so far in this drop.
    pending: Vec<PathBuf>,
}

impl DropState {
    pub fn hover(&mut self) {
        self.hovering = true;
    }

    pub fn leave(&mut self) {
        self.hovering = false;
    }

    /// Records a dropped file. `true` for the first file of a drop: the
    /// caller then waits [`GATHER`] for the others.
    pub fn dropped(&mut self, path: PathBuf) -> bool {
        self.hovering = false;
        let first = self.pending.is_empty();
        if !self.pending.contains(&path) {
            self.pending.push(path);
        }
        first
    }

    /// The files of the finished drop.
    pub fn take(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.pending)
    }
}

impl App {
    /// Whether dropped files can go anywhere: a chat is open and nothing
    /// else (a recording, an edit, the forward picker) holds the composer.
    fn can_take_drop(&self) -> bool {
        self.open.is_some()
            && self.recording_since.is_none()
            && self.editing.is_none()
            && self.forward.is_none()
    }

    /// Handles `Ui::Drop`; `Err(ui)` for every other message.
    pub(super) fn on_ui_drop(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        let Ui::Drop(d) = ui else {
            return Err(ui);
        };
        Ok(match d {
            DropMsg::Hovered => {
                self.drop.hover();
                Task::none()
            }
            DropMsg::Left => {
                self.drop.leave();
                Task::none()
            }
            DropMsg::Dropped(path) => {
                if self.drop.dropped(path) {
                    Task::perform(tokio::time::sleep(GATHER), |()| {
                        Message::Ui(Ui::Drop(DropMsg::Gathered))
                    })
                } else {
                    Task::none()
                }
            }
            DropMsg::Gathered => {
                let paths = self.drop.take();
                if paths.is_empty() {
                    return Ok(Task::none());
                }
                if !self.can_take_drop() {
                    if self.open.is_none() {
                        self.toast("Open a chat to send files.");
                    }
                    return Ok(Task::none());
                }
                let Some(chat) = self.open.as_ref().map(|c| c.chat.clone()) else {
                    return Ok(Task::none());
                };
                self.emoji_open = false;
                self.popup = None;
                Task::perform(
                    async move { paste::stage_paths(paths, false).await.map(Box::new) },
                    move |staged| Message::done(Done::Staged { chat, staged }),
                )
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_of_one_drop_are_gathered_once() {
        let mut d = DropState::default();
        d.hover();
        assert!(d.hovering);
        assert!(d.dropped("/a".into()));
        assert!(!d.hovering, "the overlay goes away on drop");
        assert!(!d.dropped("/b".into()));
        assert!(!d.dropped("/a".into()), "duplicates are ignored");
        assert_eq!(d.take(), vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(d.take().is_empty());
        // The next drop starts a new batch.
        assert!(d.dropped("/c".into()));
    }

    #[test]
    fn leaving_hides_the_overlay() {
        let mut d = DropState::default();
        d.hover();
        d.leave();
        assert!(!d.hovering);
        assert!(d.take().is_empty());
    }
}
