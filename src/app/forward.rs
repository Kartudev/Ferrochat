//! Forwarding messages, like WhatsApp Desktop: "Forward" in the message
//! menu opens the chat picker for that message; "Select messages" turns the
//! conversation into a selection (a checkbox per row, a bar instead of the
//! composer) whose messages are forwarded together.
//!
//! Up to [`MAX_FORWARD_CHATS`] chats at a time, one when a message was
//! "forwarded many times" (the official limit). Each copy is a new outgoing
//! message with the forwarded mark: it shows up at once (pending tick) in
//! every target chat and is sent in order, one message after the other.
//! Media is relayed from the same CDN blob (`WaClient::forward`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use iced::Task;

use super::message::{Done, Message, Ui};
use super::{App, store_task};
use crate::app::state::{ChatEntry, ChatList};
use crate::net::{self, ChatId, Message as ChatMessage};

/// Id of the dialog's search box (focused when it opens).
pub const FORWARD_SEARCH_ID: &str = "forward-search";

/// Most chats one forward can go to.
pub const MAX_FORWARD_CHATS: usize = 5;

/// Media older than this is re-uploaded before it is forwarded: the copy
/// relays the CDN blob, and old blobs are often gone (recipients would get
/// a broken download). An estimate; the CDN does not say when it drops one.
pub const STALE_MEDIA_SECS: i64 = 14 * 24 * 3600;

/// Whether `msg`'s media should be refreshed before forwarding it.
pub fn needs_fresh_media(msg: &ChatMessage, now: i64) -> bool {
    msg.content.media().is_some() && now - msg.timestamp > STALE_MEDIA_SECS
}

/// Messages picked in a conversation ("Select messages").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The conversation.
    pub chat: ChatId,
    /// Selected message ids, in click order.
    pub ids: Vec<String>,
}

impl Selection {
    /// The message is selected.
    pub fn contains(&self, id: &str) -> bool {
        self.ids.iter().any(|x| x == id)
    }

    /// Selects / deselects a message.
    pub fn toggle(&mut self, id: &str) {
        match self.ids.iter().position(|x| x == id) {
            Some(i) => {
                self.ids.remove(i);
            }
            None => self.ids.push(id.to_string()),
        }
    }
}

/// The "Forward message to" dialog.
#[derive(Debug, Clone)]
pub struct ForwardDialog {
    /// What is forwarded, oldest first.
    pub msgs: Vec<ChatMessage>,
    /// Search box.
    pub query: String,
    /// Chosen chats, in click order.
    pub targets: Vec<ChatId>,
}

impl ForwardDialog {
    /// A dialog for `msgs` (any order).
    pub fn new(mut msgs: Vec<ChatMessage>) -> Self {
        msgs.sort_by(|a, b| (a.timestamp, &a.id).cmp(&(b.timestamp, &b.id)));
        Self {
            msgs,
            query: String::new(),
            targets: Vec::new(),
        }
    }

    /// How many chats this forward may go to: one when a message was
    /// "forwarded many times".
    pub fn max_targets(&self) -> usize {
        if self.msgs.iter().any(ChatMessage::frequently_forwarded) {
            1
        } else {
            MAX_FORWARD_CHATS
        }
    }

    /// Adds / removes a chat; adding past the limit does nothing.
    pub fn toggle(&mut self, chat: &str) -> bool {
        if let Some(i) = self.targets.iter().position(|c| c == chat) {
            self.targets.remove(i);
            return true;
        }
        if self.targets.len() >= self.max_targets() {
            return false;
        }
        self.targets.push(chat.to_string());
        true
    }

    /// The chat is ticked.
    pub fn has(&self, chat: &str) -> bool {
        self.targets.iter().any(|c| c == chat)
    }

    /// No more chats can be ticked.
    pub fn is_full(&self) -> bool {
        self.targets.len() >= self.max_targets()
    }
}

/// Chats a forward can go to, most recent first, filtered by name or
/// number. Status / channel pseudo-chats are not offered; archived chats are.
pub fn targets<'a>(chats: &'a ChatList, query: &str) -> Vec<&'a ChatEntry> {
    let q = query.trim().to_lowercase();
    let digits: String = q.chars().filter(char::is_ascii_digit).collect();
    let mut out: Vec<&ChatEntry> = chats
        .entries
        .iter()
        .filter(|e| can_receive(&e.meta.id))
        .filter(|e| {
            q.is_empty()
                || chats.display_name(&e.meta.id).to_lowercase().contains(&q)
                || (!digits.is_empty()
                    && !e.is_group()
                    && e.meta
                        .id
                        .split('@')
                        .next()
                        .is_some_and(|u| u.contains(&digits)))
        })
        .collect();
    out.sort_by(|a, b| (b.meta.last_ts, &b.meta.id).cmp(&(a.meta.last_ts, &a.meta.id)));
    out
}

/// A chat we can send to: a contact or a group (not a status, broadcast
/// list or channel).
pub fn can_receive(jid: &str) -> bool {
    jid.ends_with("@s.whatsapp.net") || jid.ends_with("@g.us") || jid.ends_with("@lid")
}

/// `n` new message ids in ascending order: copies sent within the same
/// second keep their order in the conversation (sorted by time, then id).
pub fn ordered_ids(n: usize) -> Vec<String> {
    let mut ids: Vec<String> = (0..n).map(|_| net::new_message_id()).collect();
    ids.sort();
    ids
}

/// Forward actions (`Ui::Forward`).
#[derive(Debug, Clone)]
pub enum Fwd {
    /// "Forward" in the message menu: pick chats for this message.
    One(String),
    /// "Select messages": selection mode with this message ticked.
    Select(String),
    /// A row clicked in selection mode.
    Toggle(String),
    /// Leave selection mode.
    CancelSelect,
    /// Forward button of the selection bar.
    Selected,
    /// Search box of the dialog edited.
    Query(String),
    /// A chat ticked / unticked in the dialog.
    Target(ChatId),
    /// Send button of the dialog.
    Send,
    /// Dialog closed without sending.
    Close,
}

impl App {
    /// Handles `Ui::Forward`; `Err(ui)` for every other message.
    pub(super) fn on_ui_forward(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        let Ui::Forward(f) = ui else {
            return Err(ui);
        };
        Ok(match f {
            Fwd::One(id) => {
                self.popup = None;
                match self.find_message(&id).filter(|m| m.can_forward()) {
                    Some(m) => {
                        self.forward = Some(ForwardDialog::new(vec![m.clone()]));
                        iced::widget::operation::focus(FORWARD_SEARCH_ID)
                    }
                    None => Task::none(),
                }
            }
            Fwd::Select(id) => {
                self.popup = None;
                let Some(chat) = self.open.as_ref().map(|c| c.chat.clone()) else {
                    return Ok(Task::none());
                };
                let mut sel = Selection {
                    chat,
                    ids: Vec::new(),
                };
                if self.find_message(&id).is_some_and(ChatMessage::can_forward) {
                    sel.toggle(&id);
                }
                self.selection = Some(sel);
                self.emoji_open = false;
                Task::none()
            }
            Fwd::Toggle(id) => {
                // Unticking always works (the message may have been deleted
                // meanwhile); ticking only what can be forwarded.
                let can = self.find_message(&id).is_some_and(ChatMessage::can_forward);
                if let Some(sel) = &mut self.selection
                    && (can || sel.contains(&id))
                {
                    sel.toggle(&id);
                }
                Task::none()
            }
            Fwd::CancelSelect => {
                self.selection = None;
                Task::none()
            }
            Fwd::Selected => {
                let msgs: Vec<ChatMessage> = self
                    .selection
                    .as_ref()
                    .map(|s| {
                        s.ids
                            .iter()
                            .filter_map(|id| self.find_message(id))
                            .filter(|m| m.can_forward())
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if msgs.is_empty() {
                    return Ok(Task::none());
                }
                self.forward = Some(ForwardDialog::new(msgs));
                iced::widget::operation::focus(FORWARD_SEARCH_ID)
            }
            Fwd::Query(q) => {
                if let Some(d) = &mut self.forward {
                    d.query = q;
                }
                Task::none()
            }
            Fwd::Target(chat) => {
                if let Some(d) = &mut self.forward {
                    d.toggle(&chat);
                }
                Task::none()
            }
            Fwd::Send => self.send_forward(),
            Fwd::Close => {
                self.forward = None;
                Task::none()
            }
        })
    }

    /// Sends the dialog's messages to every ticked chat.
    fn send_forward(&mut self) -> Task<Message> {
        let Some(d) = self.forward.take() else {
            return Task::none();
        };
        if d.targets.is_empty() || d.msgs.is_empty() {
            self.forward = Some(d);
            return Task::none();
        }
        self.selection = None;
        let now = chrono::Utc::now().timestamp();
        // Stale media is re-uploaded once per source, before its first copy.
        let origins: HashMap<String, super::media_fetch::MediaOrigin> = d
            .msgs
            .iter()
            .filter(|m| needs_fresh_media(m, now))
            .map(|m| (m.id.clone(), self.media_origin(m)))
            .collect();
        let mut jobs: Vec<(ChatId, String, ChatMessage)> = Vec::new();
        let mut locals: Vec<ChatMessage> = Vec::new();
        for chat in &d.targets {
            for (src, id) in d.msgs.iter().zip(ordered_ids(d.msgs.len())) {
                let mut local = self.local_message(chat, id.clone(), src.content.clone());
                local.forwarded = Some(src.forward_score());
                locals.push(local);
                jobs.push((chat.clone(), id, src.clone()));
            }
        }
        let shown = self.add_forwarded(locals);
        let Some(client) = self.client.clone() else {
            let failed = jobs.into_iter().map(|(chat, id, _)| {
                Task::done(Message::done(Done::Sent {
                    chat,
                    id,
                    result: Err("not connected".into()),
                }))
            });
            return Task::batch(std::iter::once(shown).chain(failed));
        };
        // One after the other, so the copies arrive in order.
        use futures::StreamExt;
        let refreshed: Arc<Mutex<HashMap<String, net::MediaRef>>> = Arc::default();
        let (store, fresh_media) = (self.store.clone(), self.fresh_media.clone());
        let sends = futures::stream::iter(jobs).then(move |(chat, id, mut src)| {
            let client = client.clone();
            let origin = origins.get(&src.id).cloned();
            let refreshed = refreshed.clone();
            let (store, fresh_media) = (store.clone(), fresh_media.clone());
            async move {
                if let Some(origin) = origin {
                    let known = refreshed.lock().ok().and_then(|r| r.get(&src.id).cloned());
                    let fresh = match known {
                        Some(f) => Some(f),
                        None => match src.content.media() {
                            // Best effort: when the phone no longer has the
                            // file the old blob is relayed as before.
                            Some(old) => super::media_fetch::reupload(&client, old, &origin)
                                .await
                                .ok(),
                            None => None,
                        },
                    };
                    if let Some(f) = fresh {
                        if let Ok(mut r) = refreshed.lock() {
                            r.insert(src.id.clone(), f.clone());
                        }
                        if let Some(slot) = src.content.media_mut() {
                            *slot = f.clone();
                        }
                        // Our copy points at the fresh blob too.
                        if let Ok(mut m) = fresh_media.lock() {
                            m.insert((chat.clone(), id.clone()), f.clone());
                        }
                        let (c, i) = (chat.clone(), id.clone());
                        let _ =
                            tokio::task::spawn_blocking(move || store.set_media_ref(&c, &i, &f))
                                .await;
                    }
                }
                let result = client
                    .forward(&chat, &id, &src)
                    .await
                    .map_err(|e| e.to_string());
                Message::done(Done::Sent { chat, id, result })
            }
        });
        Task::batch([shown, Task::run(sends, |m| m)])
    }

    /// Shows and stores the local copies of a forward.
    fn add_forwarded(&mut self, locals: Vec<ChatMessage>) -> Task<Message> {
        self.chats.apply_messages(&locals, None, true, None);
        self.index_thumbs(&locals);
        let mut snap = Task::none();
        if let Some(c) = &mut self.open
            && locals.iter().any(|m| m.chat == c.chat)
        {
            if c.has_newer {
                c.replace_window(Vec::new(), true, false);
            }
            c.merge(locals.clone());
            snap = iced::widget::operation::snap_to(
                super::CONVERSATION_SCROLL,
                iced::widget::operation::RelativeOffset::START,
            );
        }
        Task::batch([
            store_task(&self.store, move |s| s.insert_messages(&locals).map(|_| ())),
            snap,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{ChatMeta, Content, ReceiptStatus};
    use crate::store::ChatRow;

    fn msg(id: &str, ts: i64, forwarded: Option<u32>) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            chat: "a@s.whatsapp.net".into(),
            sender: "a@s.whatsapp.net".into(),
            sender_name: None,
            from_me: false,
            timestamp: ts,
            content: Content::Text("hi".into()),
            status: ReceiptStatus::Read,
            expires_at: None,
            reply: None,
            edited: false,
            forwarded,
            reactions: Vec::new(),
            mentions: Vec::new(),
        }
    }

    #[test]
    fn dialog_sorts_messages_and_caps_targets() {
        let mut d = ForwardDialog::new(vec![msg("b", 20, None), msg("a", 10, None)]);
        assert_eq!(d.msgs[0].id, "a");
        assert_eq!(d.max_targets(), MAX_FORWARD_CHATS);
        for i in 0..MAX_FORWARD_CHATS {
            assert!(d.toggle(&format!("{i}@s.whatsapp.net")));
        }
        assert!(d.is_full());
        assert!(!d.toggle("x@s.whatsapp.net"));
        assert!(d.toggle("0@s.whatsapp.net"));
        assert!(!d.has("0@s.whatsapp.net"));
        assert!(d.toggle("x@s.whatsapp.net"));
    }

    #[test]
    fn frequently_forwarded_goes_to_one_chat() {
        let mut d = ForwardDialog::new(vec![msg("a", 10, Some(127)), msg("b", 11, None)]);
        assert_eq!(d.max_targets(), 1);
        assert!(d.toggle("x@s.whatsapp.net"));
        assert!(!d.toggle("y@s.whatsapp.net"));
    }

    #[test]
    fn selection_toggles() {
        let mut s = Selection {
            chat: "a".into(),
            ids: Vec::new(),
        };
        s.toggle("1");
        s.toggle("2");
        assert!(s.contains("1"));
        s.toggle("1");
        assert_eq!(s.ids, vec!["2".to_string()]);
    }

    #[test]
    fn ordered_ids_are_sorted_and_unique() {
        let ids = ordered_ids(20);
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(ids, sorted);
    }

    fn row(id: &str, name: &str, ts: i64) -> ChatRow {
        ChatRow {
            meta: ChatMeta {
                id: id.into(),
                name: Some(name.into()),
                last_ts: ts,
                ..Default::default()
            },
            last: None,
        }
    }

    #[test]
    fn targets_are_recent_first_and_searchable() {
        let mut pinned = row("34600000001@s.whatsapp.net", "Ana", 10);
        pinned.meta.pinned = true;
        let chats = ChatList::from_rows(
            vec![
                pinned,
                row("34600000002@s.whatsapp.net", "Bea", 30),
                row("123@g.us", "Family", 20),
                row("120363@newsletter", "News", 40),
                row("status@broadcast", "Status", 50),
            ],
            Default::default(),
        );
        let ids = |v: Vec<&ChatEntry>| v.iter().map(|e| e.meta.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            ids(targets(&chats, "")),
            vec![
                "34600000002@s.whatsapp.net",
                "123@g.us",
                "34600000001@s.whatsapp.net"
            ]
        );
        assert_eq!(ids(targets(&chats, "fam")), vec!["123@g.us"]);
        assert_eq!(
            ids(targets(&chats, "+34 600 000 001")),
            vec!["34600000001@s.whatsapp.net"]
        );
    }
}
