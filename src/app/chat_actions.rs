//! Chat list actions of the official client's chat menu that are not about
//! organising the list (see `lists.rs` for those): mark read / unread, mute
//! for a while, clear chat and delete chat. All of them sync to the phone
//! through app state.
//!
//! Mark and mute are optimistic (apply, persist, send, revert on failure).
//! Clear and delete cannot be undone locally, so they are sent first and
//! applied once the phone has them; without a client (tests, `--mock`
//! before login) they apply at once.

use iced::Task;

use super::message::{Done, Message, Ui};
use super::{App, store_task};
use crate::net::{ChatFlag, ChatId, MUTE_FOREVER, MUTE_OFF, MsgKey};

/// How long "Mute notifications" lasts (the official dialog's choices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MuteFor {
    /// 8 hours.
    #[default]
    Hours8,
    /// 1 week.
    Week,
    /// Until unmuted.
    Always,
}

impl MuteFor {
    /// All choices, in dialog order.
    pub const ALL: [MuteFor; 3] = [MuteFor::Hours8, MuteFor::Week, MuteFor::Always];

    /// Label in the dialog.
    pub fn label(self) -> &'static str {
        match self {
            MuteFor::Hours8 => "8 hours",
            MuteFor::Week => "1 week",
            MuteFor::Always => "Always",
        }
    }

    /// The mute end for a mute starting at `now` (Unix seconds).
    pub fn end(self, now: i64) -> i64 {
        match self {
            MuteFor::Hours8 => now + 8 * 3600,
            MuteFor::Week => now + 7 * 24 * 3600,
            MuteFor::Always => MUTE_FOREVER,
        }
    }
}

/// A confirmation dialog for a chat action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatDialog {
    /// "Mute notifications" with the chosen duration.
    Mute {
        /// Chat.
        chat: ChatId,
        /// Selected duration.
        choice: MuteFor,
    },
    /// "Clear this chat?".
    Clear {
        /// Chat.
        chat: ChatId,
        /// "Keep starred messages" ticked.
        keep_starred: bool,
    },
    /// "Delete this chat?".
    Delete {
        /// Chat.
        chat: ChatId,
    },
}

/// Chat action messages (`Ui::ChatAct`).
#[derive(Debug, Clone)]
pub enum ChatAct {
    /// "Mark as unread".
    MarkUnread(ChatId),
    /// "Mark as read".
    MarkRead(ChatId),
    /// "Mute notifications…": open the duration dialog.
    AskMute(ChatId),
    /// A duration picked in the mute dialog.
    PickMute(MuteFor),
    /// "Mute" in the dialog.
    ConfirmMute,
    /// "Unmute notifications".
    Unmute(ChatId),
    /// "Clear chat": ask first.
    AskClear(ChatId),
    /// "Keep starred messages" toggled.
    KeepStarred(bool),
    /// "Clear chat" confirmed.
    ConfirmClear,
    /// "Delete chat": ask first.
    AskDelete(ChatId),
    /// "Delete chat" confirmed.
    ConfirmDelete,
    /// Dialog cancelled.
    Cancel,
}

/// Results of chat action tasks (`Done::ChatAct`).
#[derive(Debug, Clone)]
pub enum ChatActDone {
    /// Syncing "mark as read / unread" failed: restore the badge.
    MarkFailed {
        /// Chat.
        chat: ChatId,
        /// Badge before the change.
        unread: u32,
        /// The change was "unread".
        unread_mark: bool,
        /// Error text.
        error: String,
    },
    /// The phone has the "clear chat" (or it failed).
    Cleared {
        /// Chat.
        chat: ChatId,
        /// Starred messages stay.
        keep_starred: bool,
        /// Outcome.
        result: Result<(), String>,
    },
    /// The phone has the "delete chat" (or it failed).
    Deleted {
        /// Chat.
        chat: ChatId,
        /// Outcome.
        result: Result<(), String>,
    },
}

impl App {
    /// Handles `Ui::ChatAct`; `Err(ui)` for every other message.
    pub(super) fn on_ui_chat_actions(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        let Ui::ChatAct(act) = ui else {
            return Err(ui);
        };
        self.sidebar_menu = None;
        Ok(match act {
            ChatAct::MarkUnread(chat) => self.mark_chat(chat, false),
            ChatAct::MarkRead(chat) => self.mark_chat(chat, true),
            ChatAct::AskMute(chat) => {
                self.chat_dialog = Some(ChatDialog::Mute {
                    chat,
                    choice: MuteFor::default(),
                });
                Task::none()
            }
            ChatAct::PickMute(c) => {
                if let Some(ChatDialog::Mute { choice, .. }) = &mut self.chat_dialog {
                    *choice = c;
                }
                Task::none()
            }
            ChatAct::ConfirmMute => match self.chat_dialog.take() {
                Some(ChatDialog::Mute { chat, choice }) => {
                    self.set_mute(chat, choice.end(super::now_secs()))
                }
                other => {
                    self.chat_dialog = other;
                    Task::none()
                }
            },
            ChatAct::Unmute(chat) => self.set_mute(chat, MUTE_OFF),
            ChatAct::AskClear(chat) => {
                self.chat_dialog = Some(ChatDialog::Clear {
                    chat,
                    keep_starred: true,
                });
                Task::none()
            }
            ChatAct::KeepStarred(v) => {
                if let Some(ChatDialog::Clear { keep_starred, .. }) = &mut self.chat_dialog {
                    *keep_starred = v;
                }
                Task::none()
            }
            ChatAct::ConfirmClear => match self.chat_dialog.take() {
                Some(ChatDialog::Clear { chat, keep_starred }) => {
                    self.clear_chat(chat, keep_starred)
                }
                other => {
                    self.chat_dialog = other;
                    Task::none()
                }
            },
            ChatAct::AskDelete(chat) => {
                self.chat_dialog = Some(ChatDialog::Delete { chat });
                Task::none()
            }
            ChatAct::ConfirmDelete => match self.chat_dialog.take() {
                Some(ChatDialog::Delete { chat }) => self.delete_chat(chat),
                other => {
                    self.chat_dialog = other;
                    Task::none()
                }
            },
            ChatAct::Cancel => {
                self.chat_dialog = None;
                Task::none()
            }
        })
    }

    pub(super) fn on_chat_act_done(&mut self, done: ChatActDone) -> Task<Message> {
        match done {
            ChatActDone::MarkFailed {
                chat,
                unread,
                unread_mark,
                error,
            } => {
                tracing::warn!(error = %error, "mark chat sync failed");
                self.toast("Could not sync this change with your phone.");
                if unread_mark {
                    self.chats.mark_read(&chat);
                }
                self.chats.set_unread(&chat, unread);
                store_task(&self.store, move |s| s.set_unread(&chat, unread))
            }
            ChatActDone::Cleared {
                chat,
                keep_starred,
                result: Ok(()),
            } => self.apply_clear(chat, keep_starred),
            ChatActDone::Deleted {
                chat,
                result: Ok(()),
            } => self.apply_delete(chat),
            ChatActDone::Cleared {
                result: Err(error), ..
            } => {
                tracing::warn!(error = %error, "clear chat failed");
                self.toast("Could not clear this chat. Check your connection.");
                Task::none()
            }
            ChatActDone::Deleted {
                result: Err(error), ..
            } => {
                tracing::warn!(error = %error, "delete chat failed");
                self.toast("Could not delete this chat. Check your connection.");
                Task::none()
            }
        }
    }

    /// The newest message of a chat as the action's message range.
    fn last_key(&self, chat: &str) -> Option<(MsgKey, i64)> {
        let last = self.chats.get(chat)?.last.as_ref()?;
        Some((last.key(), last.timestamp))
    }

    /// Optimistic "mark as read / unread".
    fn mark_chat(&mut self, chat: ChatId, read: bool) -> Task<Message> {
        let unread = self.chats.get(&chat).map_or(0, |e| e.meta.unread);
        if read {
            self.chats.mark_read(&chat);
        } else {
            self.chats.mark_unread(&chat);
        }
        let now = self.chats.get(&chat).map_or(0, |e| e.meta.unread);
        let persist = {
            let chat = chat.clone();
            store_task(&self.store, move |s| s.set_unread(&chat, now))
        };
        Task::batch([persist, self.sync_read_mark(chat, read, unread)])
    }

    /// Tells the other devices that a chat was read / marked unread;
    /// `unread` is the badge to restore if that fails.
    pub(super) fn sync_read_mark(&self, chat: ChatId, read: bool, unread: u32) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let last = self.last_key(&chat);
        Task::perform(
            async move {
                let r = client.mark_chat_read(&chat, read, last.as_ref()).await;
                (chat, r.map_err(|e| e.to_string()))
            },
            move |(chat, r)| match r {
                Ok(()) => Message::done(Done::Nothing),
                Err(error) => Message::done(Done::ChatAct(ChatActDone::MarkFailed {
                    chat,
                    unread,
                    unread_mark: !read,
                    error,
                })),
            },
        )
    }

    /// Optimistic mute until `end` (or unmute with [`MUTE_OFF`]): apply,
    /// persist, send, revert on failure.
    pub(super) fn set_mute(&mut self, chat: ChatId, end: i64) -> Task<Message> {
        let undo = ChatFlag::Muted(self.chats.get(&chat).map_or(MUTE_OFF, |e| e.meta.mute_end));
        let flag = ChatFlag::Muted(end);
        self.chats.apply_flag(&chat, flag);
        let persist = {
            let chat = chat.clone();
            store_task(&self.store, move |s| s.set_chat_flag(&chat, flag))
        };
        let Some(client) = self.client.clone() else {
            return persist;
        };
        let send = Task::perform(
            async move {
                let r = client.set_mute(&chat, end).await.map_err(|e| e.to_string());
                (chat, r)
            },
            move |(chat, r)| match r {
                Ok(()) => Message::done(Done::Nothing),
                Err(error) => Message::done(Done::FlagFailed { chat, undo, error }),
            },
        );
        Task::batch([persist, send])
    }

    fn clear_chat(&mut self, chat: ChatId, keep_starred: bool) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return self.apply_clear(chat, keep_starred);
        };
        let last = self.last_key(&chat);
        Task::perform(
            async move {
                let r = client.clear_chat(&chat, keep_starred, last.as_ref()).await;
                (chat, r.map_err(|e| e.to_string()))
            },
            move |(chat, result)| {
                Message::done(Done::ChatAct(ChatActDone::Cleared {
                    chat,
                    keep_starred,
                    result,
                }))
            },
        )
    }

    fn delete_chat(&mut self, chat: ChatId) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return self.apply_delete(chat);
        };
        let last = self.last_key(&chat);
        Task::perform(
            async move {
                let r = client.delete_chat(&chat, last.as_ref()).await;
                (chat, r.map_err(|e| e.to_string()))
            },
            |(chat, result)| Message::done(Done::ChatAct(ChatActDone::Deleted { chat, result })),
        )
    }

    /// Clears a chat here (our action or another device's). Starred
    /// messages stay in the open conversation when kept.
    pub(super) fn apply_clear(&mut self, chat: ChatId, keep_starred: bool) -> Task<Message> {
        self.chats.clear_chat(&chat);
        if !keep_starred {
            self.forget_stars(&chat);
        }
        if let Some(c) = &mut self.open
            && c.chat == chat
        {
            if keep_starred {
                let stars = &self.nav.star_set;
                c.messages
                    .retain(|m| stars.contains(&(chat.clone(), m.id.clone())));
            } else {
                c.messages.clear();
            }
            c.has_more = false;
            c.has_newer = false;
            self.pending_read = 0;
            self.popup = None;
            self.dialog = None;
            self.reply_to = None;
        }
        store_task(&self.store, move |s| s.clear_chat(&chat, keep_starred))
    }

    /// Deletes a chat here (our action or another device's).
    pub(super) fn apply_delete(&mut self, chat: ChatId) -> Task<Message> {
        self.chats.delete_chat(&chat);
        self.forget_stars(&chat);
        if self.open.as_ref().is_some_and(|c| c.chat == chat) {
            self.open = None;
            self.info = None;
            self.msg_info = None;
            self.popup = None;
            self.dialog = None;
            self.reply_to = None;
            self.chat_search = None;
            self.pending_read = 0;
        }
        self.drafts.take(&chat);
        store_task(&self.store, move |s| s.delete_chat(&chat))
    }
}

impl App {
    /// Drops a chat's messages from the starred set and list.
    fn forget_stars(&mut self, chat: &str) {
        self.nav.star_set.retain(|(c, _)| c != chat);
        self.nav.starred.retain(|s| s.message.chat != chat);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_durations_match_the_official_dialog() {
        assert_eq!(MuteFor::Hours8.end(1000), 1000 + 28_800);
        assert_eq!(MuteFor::Week.end(1000), 1000 + 604_800);
        assert_eq!(MuteFor::Always.end(1000), MUTE_FOREVER);
        assert_eq!(MuteFor::default(), MuteFor::Hours8);
        let labels: Vec<_> = MuteFor::ALL.iter().map(|m| m.label()).collect();
        assert_eq!(labels, ["8 hours", "1 week", "Always"]);
    }
}
