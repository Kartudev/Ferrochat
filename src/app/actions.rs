//! User actions and background results added on top of the core update loop:
//! replies, reactions, edits and deletes, search and jumping, profile
//! pictures, typing / presence and group receipts.

use std::time::{Instant, SystemTime};

use iced::Task;
use iced::advanced::widget::{self, Operation, operation::Outcome};
use iced::widget::operation;
use iced::{Rectangle, Vector};

use super::message::{Done, Located, Message, Ui};
use super::{
    App, CHAT_SEARCH_ID, COMPOSER_ID, ChatSearch, Dialog, HIGHLIGHT_FOR, MAX_AVATAR_FETCHES,
    MAX_AVATAR_QUEUE, MAX_DEFERRED, MAX_VIDEO_FILES, PAGE, Popup, blocking, now_secs, store_task,
};
use super::{CONVERSATION_SCROLL, live};
use crate::media::avatar;
use crate::net::{
    self, ChatState, ClientHandle, Content, Event, Message as ChatMessage, MsgKey, Quote,
    ReceiptStatus,
};
use crate::store::SearchHit;

/// Finds a message row and the geometry of the conversation scrollable.
struct Locate {
    target: widget::Id,
    scroll: widget::Id,
    found: Option<Rectangle>,
    area: Option<(Rectangle, Rectangle, Vector)>,
}

/// Where a popup card goes relative to the conversation viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopupPos {
    /// Top of the message row inside the viewport.
    pub top: f32,
    /// Height of the row.
    pub height: f32,
    /// Viewport height.
    pub viewport: f32,
}

/// Top offset of a popup card of `card_h` for a row at `pos`: below the
/// row if it fits, else above it, always inside the viewport.
pub fn popup_top(pos: PopupPos, card_h: f32) -> f32 {
    let below = pos.top + pos.height + 4.0;
    let y = if below + card_h <= pos.viewport {
        below
    } else {
        pos.top - card_h - 4.0
    };
    y.clamp(4.0, (pos.viewport - card_h - 4.0).max(4.0))
}

impl Operation<Option<Located>> for Locate {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Option<Located>>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        if id == Some(&self.target) {
            self.found = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&widget::Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn widget::operation::Scrollable,
    ) {
        if id == Some(&self.scroll) {
            self.area = Some((bounds, content_bounds, translation));
        }
    }

    fn finish(&self) -> Outcome<Option<Located>> {
        Outcome::Some(match (self.found, self.area) {
            (Some(f), Some((viewport, content, translation))) => Some(Located {
                top: f.y - content.y,
                height: f.height,
                viewport: viewport.height,
                content: content.height,
                screen_top: f.y - translation.y - viewport.y,
            }),
            _ => None,
        })
    }
}

/// Container id of a message row.
pub fn row_id(id: &str) -> widget::Id {
    widget::Id::from(format!("msg-{id}"))
}

/// Scroll offset (distance from the newest end, which is the anchor) that
/// centres a message in the viewport.
pub fn jump_offset(l: Located) -> f32 {
    let max_top = (l.content - l.viewport).max(0.0);
    let top = (l.top + l.height / 2.0 - l.viewport / 2.0).clamp(0.0, max_top);
    max_top - top
}

impl App {
    fn own(&self) -> String {
        self.own_jid.clone().unwrap_or_default()
    }

    fn open_chat_id(&self) -> Option<String> {
        self.open.as_ref().map(|c| c.chat.clone())
    }

    fn client_task<F, Fut>(&self, f: F) -> Task<Message>
    where
        F: FnOnce(ClientHandle) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        match self.client.clone() {
            Some(c) => Task::perform(f(c), |()| Message::done(Done::Nothing)),
            None => Task::none(),
        }
    }

    // ------------------------------------------------------------ dispatch

    /// Handles the UI messages added in this module. Returns `Err(ui)` for
    /// variants that belong to the core update loop.
    pub(super) fn on_ui_extra(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        Ok(match ui {
            // Screenshots script their own hover; a stray X pointer must not move it.
            Ui::HoverMessage(_) | Ui::UnhoverMessage(_) if self.cfg.screenshot.is_some() => {
                Task::none()
            }
            Ui::HoverMessage(id) => {
                self.hovered = Some(id);
                Task::none()
            }
            Ui::UnhoverMessage(id) => {
                if self.hovered.as_deref() == Some(id.as_str()) {
                    self.hovered = None;
                }
                Task::none()
            }
            Ui::ClosePopup => {
                self.popup = None;
                Task::none()
            }
            Ui::ToggleReactionPicker(id) => self.toggle_popup(id, Popup::Reactions),
            Ui::ToggleMessageMenu(id) => self.toggle_popup(id, Popup::Menu),
            Ui::React { id, emoji } => self.react(&id, emoji),
            Ui::StartReply(id) => self.start_reply(&id),
            Ui::CancelReply => {
                self.reply_to = None;
                Task::none()
            }
            Ui::JumpTo(id) => self.jump_to(id),
            Ui::StartEdit(id) => self.start_edit(&id),
            Ui::CancelEdit => {
                self.cancel_edit();
                Task::none()
            }
            Ui::AskDelete(id) => {
                self.popup = None;
                self.dialog = Some(Dialog::Delete(id));
                Task::none()
            }
            Ui::ShowReactions(id) => {
                self.popup = None;
                self.dialog = Some(Dialog::Reactions(id));
                Task::none()
            }
            Ui::CloseDialog => {
                self.dialog = None;
                Task::none()
            }
            Ui::ConfirmDelete { id, everyone } => {
                self.dialog = None;
                self.delete_message(&id, everyone)
            }
            Ui::CopyText(id) => {
                self.popup = None;
                match self.find_message(&id).map(|m| m.content.clone()) {
                    Some(Content::Text(t)) => iced::clipboard::write(t),
                    Some(c) => iced::clipboard::write(c.preview()),
                    None => Task::none(),
                }
            }
            Ui::SidebarScrolled { offset_y, height } => {
                let row = self.theme.metrics.row_height;
                let first = ((offset_y / row).floor() as isize - 2).max(0) as usize;
                let last = ((offset_y + height) / row).ceil() as usize + 3;
                self.sidebar_rows = (first, last);
                self.want_visible_avatars()
            }
            Ui::ToggleChatSearch => self.toggle_chat_search(),
            Ui::ChatSearch(q) => self.chat_search_input(q),
            Ui::ChatSearchStep { delta } => self.chat_search_step(delta),
            Ui::OpenHit { chat, id } => {
                let open = self.open_chat(chat);
                Task::batch([open, self.jump_to(id)])
            }
            Ui::JumpLatest => self.jump_latest(),
            other => match self.on_ui_video(other) {
                Ok(t) => t,
                Err(other) => match self.on_ui_viewer(other) {
                    Ok(t) => t,
                    Err(other) => match self.on_ui_info(other) {
                        Ok(t) => t,
                        Err(other) => return self.on_ui_lists(other),
                    },
                },
            },
        })
    }

    fn toggle_popup(&mut self, id: String, which: Popup) -> Task<Message> {
        self.popup_pos = None;
        self.popup = match &self.popup {
            Some((cur, p)) if *cur == id && *p == which => None,
            _ => Some((id.clone(), which)),
        };
        if self.popup.is_none() {
            return Task::none();
        }
        // Where is the row on screen? The card floats there (it does not
        // push the conversation around).
        let target = row_id(&id);
        widget::operate(Locate {
            target,
            scroll: widget::Id::new(CONVERSATION_SCROLL),
            found: None,
            area: None,
        })
        .map(move |loc| {
            Message::done(Done::PopupAt {
                id: id.clone(),
                loc,
            })
        })
    }

    pub(super) fn on_popup_at(&mut self, id: String, loc: Option<Located>) {
        if self.popup.as_ref().map(|(i, _)| i.as_str()) != Some(id.as_str()) {
            return;
        }
        self.popup_pos = loc.map(|l| PopupPos {
            top: l.screen_top,
            height: l.height,
            viewport: l.viewport,
        });
        if self.popup_pos.is_none() {
            self.popup = None;
        }
    }

    // ------------------------------------------------------------ reactions

    fn react(&mut self, id: &str, emoji: String) -> Task<Message> {
        self.popup = None;
        let Some(m) = self.find_message(id).cloned() else {
            return Task::none();
        };
        if matches!(m.content, Content::Deleted) {
            return Task::none();
        }
        let me = self.own();
        if me.is_empty() {
            self.toast("Not connected yet.");
            return Task::none();
        }
        let previous = m
            .reactions
            .iter()
            .find(|r| r.reactor == me)
            .map(|r| r.emoji.clone());
        // Tapping your own emoji again removes it, like the official client.
        let new = if previous.as_deref() == Some(emoji.as_str()) {
            String::new()
        } else {
            emoji
        };
        let ts = now_secs();
        if let Some(c) = &mut self.open {
            c.apply_reaction(id, &me, &new, ts);
        }
        let (chat, mid, reactor, e2) = (m.chat.clone(), m.id.clone(), me, new.clone());
        let persist = store_task(&self.store, move |s| {
            s.apply_reaction(&chat, &mid, &reactor, &e2, ts).map(|_| ())
        });
        let Some(client) = self.client.clone() else {
            return persist;
        };
        let key = m.key();
        let send = Task::perform(
            async move {
                let r = client
                    .send_reaction(&key, &new)
                    .await
                    .map_err(|e| e.to_string());
                (key, r)
            },
            move |(key, result)| {
                Message::done(Done::ReactionSent {
                    chat: key.chat,
                    id: key.id,
                    previous,
                    result,
                })
            },
        );
        Task::batch([persist, send])
    }

    pub(super) fn on_reaction_sent(
        &mut self,
        chat: String,
        id: String,
        previous: Option<String>,
        result: Result<(), String>,
    ) -> Task<Message> {
        let Err(e) = result else {
            return Task::none();
        };
        tracing::warn!(error = %e, "reaction failed");
        self.toast("Could not send the reaction.");
        let me = self.own();
        let restore = previous.unwrap_or_default();
        let ts = now_secs();
        if let Some(c) = &mut self.open
            && c.chat == chat
        {
            c.apply_reaction(&id, &me, &restore, ts);
        }
        store_task(&self.store, move |s| {
            s.apply_reaction(&chat, &id, &me, &restore, ts).map(|_| ())
        })
    }

    // -------------------------------------------------------------- replies

    fn start_reply(&mut self, id: &str) -> Task<Message> {
        self.popup = None;
        let Some(m) = self.find_message(id).cloned() else {
            return Task::none();
        };
        if matches!(m.content, Content::Deleted | Content::ViewOnce) {
            return Task::none();
        }
        self.cancel_edit();
        self.reply_to = Some(Quote::of(&m));
        operation::focus(COMPOSER_ID)
    }

    // ---------------------------------------------------------------- edits

    fn start_edit(&mut self, id: &str) -> Task<Message> {
        self.popup = None;
        let Some(m) = self.find_message(id).cloned() else {
            return Task::none();
        };
        if !m.can_edit(now_secs()) {
            self.toast("Messages can only be edited within 15 minutes of sending.");
            return Task::none();
        }
        let Content::Text(t) = &m.content else {
            return Task::none();
        };
        self.reply_to = None;
        self.composer.set_text(t);
        self.editing = Some(m.id);
        operation::focus(COMPOSER_ID)
    }

    /// Keeps the composer text of the open chat as its draft.
    pub(super) fn stash_draft(&mut self) {
        if self.editing.is_some() {
            return;
        }
        if let Some(chat) = self.open.as_ref().map(|c| c.chat.clone()) {
            self.drafts.save(&chat, self.composer.text());
        }
    }

    /// An emoji was picked: insert it at the cursor and remember it.
    pub(super) fn pick_emoji(&mut self, e: &str) -> Task<Message> {
        self.composer.insert(e);
        crate::app::emoji::push_recent(&mut self.emoji.recent, e);
        let json = serde_json::to_string(&self.emoji.recent).unwrap_or_default();
        Task::batch([
            store_task(&self.store, move |s| {
                s.set_kv(crate::app::emoji::RECENT_KEY, &json)
            }),
            self.compose_input(),
        ])
    }

    /// Category tab: scroll the picker grid to that section.
    pub(super) fn scroll_picker_to(&mut self, c: crate::app::emoji::Category) -> Task<Message> {
        use crate::app::emoji;
        self.emoji.query.clear();
        let m = self.theme.metrics;
        let pane = self.window_size().width - m.rail_width - m.sidebar_width;
        let sections = self.emoji.sections();
        let y = emoji::section_offset(
            &sections,
            c,
            emoji::columns(pane),
            emoji::CELL,
            emoji::HEADER,
        )
        .unwrap_or(0.0);
        operation::scroll_to(emoji::SCROLL_ID, operation::AbsoluteOffset { x: 0.0, y })
    }

    pub(super) fn cancel_edit(&mut self) {
        if self.editing.take().is_some() {
            self.composer.clear();
        }
    }

    /// Enter pressed while editing.
    pub(super) fn send_edit(&mut self) -> Task<Message> {
        let Some(id) = self.editing.take() else {
            return Task::none();
        };
        let text = self.composer.text().trim().to_string();
        self.composer.clear();
        let Some(chat) = self.open_chat_id() else {
            return Task::none();
        };
        let Some(m) = self.find_message(&id).cloned() else {
            return Task::none();
        };
        if text.is_empty() || Content::Text(text.clone()) == m.content {
            return Task::none();
        }
        if !m.can_edit(now_secs()) {
            self.toast("The 15 minute edit window has passed.");
            return Task::none();
        }
        let content = Content::Text(text.clone());
        let me = self.own();
        if let Some(c) = &mut self.open {
            c.apply_edit(&id, &me, true, &content);
        }
        self.chats.apply_edit(&chat, &id, &me, true, &content);
        let (c2, i2, me2, content2) = (chat.clone(), id.clone(), me, content);
        let persist = store_task(&self.store, move |s| {
            s.apply_edit(&c2, &i2, &me2, true, &content2).map(|_| ())
        });
        let Some(client) = self.client.clone() else {
            return persist;
        };
        let send = Task::perform(
            async move {
                client
                    .edit_text(&chat, &id, &text)
                    .await
                    .map_err(|e| e.to_string())
            },
            |result| {
                Message::done(Done::ActionDone {
                    what: "edit",
                    result,
                })
            },
        );
        Task::batch([persist, send])
    }

    // -------------------------------------------------------------- deletes

    fn delete_message(&mut self, id: &str, everyone: bool) -> Task<Message> {
        let Some(m) = self.find_message(id).cloned() else {
            return Task::none();
        };
        let now = now_secs();
        if everyone {
            if !m.can_revoke(now) {
                self.toast("Messages can only be deleted for everyone within about 2 days.");
                return Task::none();
            }
            let deleted = ChatMessage {
                content: Content::Deleted,
                reactions: Vec::new(),
                reply: None,
                ..m.clone()
            };
            self.chats
                .apply_messages(std::slice::from_ref(&deleted), None, true, None);
            if let Some(c) = &mut self.open {
                c.merge(vec![deleted.clone()]);
            }
            let persist = store_task(&self.store, move |s| {
                s.insert_messages(&[deleted]).map(|_| ())
            });
            let Some(client) = self.client.clone() else {
                return persist;
            };
            let (chat, mid) = (m.chat.clone(), m.id.clone());
            let send = Task::perform(
                async move { client.revoke(&chat, &mid).await.map_err(|e| e.to_string()) },
                |result| {
                    Message::done(Done::ActionDone {
                        what: "delete for everyone",
                        result,
                    })
                },
            );
            return Task::batch([persist, send]);
        }
        let local = self.remove_locally(&m.chat, &m.id);
        let Some(client) = self.client.clone() else {
            return local;
        };
        let key = m.key();
        let ts = m.timestamp;
        let sync = Task::perform(
            async move {
                client
                    .delete_for_me(&key, ts)
                    .await
                    .map_err(|e| e.to_string())
            },
            |result| {
                Message::done(Done::ActionDone {
                    what: "delete for me",
                    result,
                })
            },
        );
        Task::batch([local, sync])
    }

    /// Drops a message from the window, the cache and (if needed) the chat
    /// list preview.
    pub(super) fn remove_locally(&mut self, chat: &str, id: &str) -> Task<Message> {
        if let Some(c) = &mut self.open
            && c.chat == chat
        {
            c.remove(id);
        }
        let was_last = self.chats.is_last(chat, id);
        let (c2, i2) = (chat.to_string(), id.to_string());
        let s = self.store.clone();
        let c3 = chat.to_string();
        blocking(
            move || {
                s.delete_message(&c2, &i2).map_err(|e| e.to_string())?;
                if was_last {
                    Ok(s.load_messages(&c2, None, 1)
                        .map_err(|e| e.to_string())?
                        .pop())
                } else {
                    Ok::<_, String>(None)
                }
            },
            move |r| match r {
                Ok(last) if was_last => Message::done(Done::LastMessage {
                    chat: c3,
                    last: last.map(Box::new),
                }),
                Ok(_) => Message::done(Done::Nothing),
                Err(e) => Message::done(Done::Stored(Err(e))),
            },
        )
    }

    pub(super) fn on_action_done(&mut self, what: &'static str, result: Result<(), String>) {
        if let Err(e) = result {
            tracing::warn!(error = %e, what, "action failed");
            self.toast(match what {
                "edit" => "Could not edit the message. Try again.",
                "delete for everyone" => "Could not delete the message for everyone.",
                _ => "Deleted here, but it could not be synced to your other devices.",
            });
        }
    }

    // --------------------------------------------------------------- search

    /// Chat list query changed: run the message search.
    pub(super) fn run_list_search(&mut self) -> Task<Message> {
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        let q = self.search.trim().to_string();
        if q.chars().count() < 2 {
            self.message_hits.clear();
            return Task::none();
        }
        let s = self.store.clone();
        blocking(
            move || s.search_messages(None, &q, 50).unwrap_or_default(),
            move |hits| Message::done(Done::ListSearch { generation, hits }),
        )
    }

    fn toggle_chat_search(&mut self) -> Task<Message> {
        if self.chat_search.take().is_some() {
            return Task::none();
        }
        self.chat_search = Some(ChatSearch::default());
        operation::focus(CHAT_SEARCH_ID)
    }

    fn chat_search_input(&mut self, q: String) -> Task<Message> {
        let Some(chat) = self.open_chat_id() else {
            return Task::none();
        };
        let Some(cs) = &mut self.chat_search else {
            return Task::none();
        };
        cs.query = q.clone();
        cs.generation = cs.generation.wrapping_add(1);
        let generation = cs.generation;
        if q.trim().chars().count() < 2 {
            cs.hits.clear();
            cs.index = 0;
            return Task::none();
        }
        let s = self.store.clone();
        blocking(
            move || {
                let hits = s
                    .search_messages(Some(&chat), &q, crate::store::MAX_SEARCH_HITS)
                    .unwrap_or_default();
                (chat, hits)
            },
            move |(chat, hits)| {
                Message::done(Done::ChatSearch {
                    generation,
                    chat,
                    hits,
                })
            },
        )
    }

    pub(super) fn on_chat_search_results(
        &mut self,
        generation: u32,
        chat: String,
        hits: Vec<SearchHit>,
    ) -> Task<Message> {
        if self.open_chat_id().as_deref() != Some(chat.as_str()) {
            return Task::none();
        }
        let Some(cs) = &mut self.chat_search else {
            return Task::none();
        };
        if cs.generation != generation {
            return Task::none();
        }
        cs.hits = hits;
        cs.index = 0;
        match cs.hits.first().map(|h| h.id.clone()) {
            Some(id) => self.jump_to(id),
            None => Task::none(),
        }
    }

    fn chat_search_step(&mut self, delta: i32) -> Task<Message> {
        let Some(cs) = &mut self.chat_search else {
            return Task::none();
        };
        if cs.hits.is_empty() {
            return Task::none();
        }
        let n = cs.hits.len() as i64;
        cs.index = (cs.index as i64 + i64::from(delta)).rem_euclid(n) as usize;
        let id = cs.hits[cs.index].id.clone();
        self.jump_to(id)
    }

    // ----------------------------------------------------------------- jump

    /// Brings `id` into view: scroll if loaded, otherwise load the cache
    /// around it first.
    pub(super) fn jump_to(&mut self, id: String) -> Task<Message> {
        self.popup = None;
        let Some(chat) = self.open_chat_id() else {
            return Task::none();
        };
        if self.open.as_ref().is_some_and(|c| c.get(&id).is_some()) {
            return self.scroll_to_message(id);
        }
        let s = self.store.clone();
        let anchor = id.clone();
        blocking(
            move || {
                let m = s.message(&chat, &id).ok().flatten()?;
                let w = s
                    .load_around(&chat, m.timestamp, &id, PAGE / 2, PAGE / 2)
                    .ok()?;
                Some((chat, w))
            },
            move |r| match r {
                Some((chat, window)) => Message::done(Done::Window {
                    chat,
                    anchor,
                    window,
                }),
                None => Message::done(Done::ActionDone {
                    what: "jump",
                    result: Err("original message is not in the local history".into()),
                }),
            },
        )
    }

    pub(super) fn on_window(
        &mut self,
        chat: String,
        anchor: String,
        window: crate::store::Window,
    ) -> Task<Message> {
        let Some(c) = &mut self.open else {
            return Task::none();
        };
        if c.chat != chat {
            return Task::none();
        }
        self.index_thumbs(&window.messages);
        let Some(c) = &mut self.open else {
            return Task::none();
        };
        c.replace_window(window.messages, window.has_older, window.has_newer);
        let auto = self.auto_download_images();
        if anchor.is_empty() {
            return Task::batch([
                auto,
                operation::snap_to(CONVERSATION_SCROLL, operation::RelativeOffset::START),
            ]);
        }
        Task::batch([auto, self.scroll_to_message(anchor)])
    }

    /// Highlights a message and scrolls it to the middle of the viewport.
    fn scroll_to_message(&mut self, id: String) -> Task<Message> {
        self.highlight = Some((id.clone(), Instant::now()));
        let target = row_id(&id);
        // The window may have just been replaced: give the layout a frame.
        Task::perform(
            tokio::time::sleep(std::time::Duration::from_millis(40)),
            |()| (),
        )
        .then(move |()| {
            widget::operate(Locate {
                target: target.clone(),
                scroll: widget::Id::new(CONVERSATION_SCROLL),
                found: None,
                area: None,
            })
        })
        .map(|l| Message::done(Done::Located(l)))
    }

    pub(super) fn on_located(&mut self, loc: Option<Located>) -> Task<Message> {
        match loc {
            Some(l) => operation::scroll_to(
                CONVERSATION_SCROLL,
                operation::AbsoluteOffset {
                    x: None,
                    y: Some(jump_offset(l)),
                },
            ),
            None => Task::none(),
        }
    }

    fn jump_latest(&mut self) -> Task<Message> {
        let Some(chat) = self.open_chat_id() else {
            return Task::none();
        };
        let s = self.store.clone();
        let c2 = chat.clone();
        blocking(
            move || s.load_messages(&c2, None, PAGE).map(|m| (c2, m)),
            |r| match r {
                Ok((chat, msgs)) => Message::done(Done::Window {
                    anchor: String::new(),
                    window: crate::store::Window {
                        has_older: msgs.len() == PAGE,
                        has_newer: false,
                        messages: msgs,
                    },
                    chat,
                }),
                Err(e) => Message::done(Done::Stored(Err(e.to_string()))),
            },
        )
    }

    /// Near the newest end of a window opened in the middle of history.
    pub(super) fn load_newer_page(&mut self) -> Task<Message> {
        let Some(c) = &self.open else {
            return Task::none();
        };
        if !c.has_newer {
            return Task::none();
        }
        let Some(last) = c.messages.back() else {
            return Task::none();
        };
        let (chat, ts, id) = (c.chat.clone(), last.timestamp, last.id.clone());
        if let Some(c) = &mut self.open {
            c.loading = true;
        }
        let s = self.store.clone();
        blocking(
            move || s.load_newer(&chat, (ts, &id), PAGE).map(|r| (chat, r)),
            |r| match r {
                Ok((chat, (msgs, more))) => Message::done(Done::Newer {
                    chat,
                    msgs,
                    has_newer: more,
                }),
                Err(e) => Message::done(Done::Stored(Err(e.to_string()))),
            },
        )
    }

    pub(super) fn on_newer(&mut self, chat: String, msgs: Vec<ChatMessage>, has_newer: bool) {
        self.index_thumbs(&msgs);
        if let Some(c) = &mut self.open
            && c.chat == chat
        {
            c.append_newer(msgs, has_newer);
        }
    }

    // ------------------------------------------------------------- avatars

    /// JIDs of everything whose picture is on screen right now: the visible
    /// chat rows, the open chat and ourselves.
    fn visible_avatar_jids(&self) -> Vec<String> {
        let (first, last) = self.sidebar_rows;
        let mut v: Vec<String> = Vec::new();
        if let Some(c) = &self.open {
            v.push(c.chat.clone());
        }
        if let Some(me) = &self.own_jid {
            v.push(me.clone());
        }
        v.extend(
            self.chats
                .filtered_view(&self.search, &self.filter, self.archived_view)
                .into_iter()
                .skip(first)
                .take(last.saturating_sub(first) + 1)
                .map(|e| e.meta.id.clone()),
        );
        v
    }

    pub(super) fn want_visible_avatars(&mut self) -> Task<Message> {
        let jids = self.visible_avatar_jids();
        self.want_avatars(jids)
    }

    /// Queues lookups for pictures we do not have.
    pub(super) fn want_avatars(&mut self, jids: Vec<String>) -> Task<Message> {
        for jid in jids.into_iter().rev() {
            if self.avatars.contains(&jid)
                || self.avatar_none.contains(&jid)
                || self.avatar_inflight.contains(&jid)
                || jid.ends_with("@broadcast")
                || jid.ends_with("@newsletter")
            {
                continue;
            }
            self.avatar_queue.retain(|q| *q != jid);
            // Front: the list is walked in reverse so the first row wins.
            self.avatar_queue.push_front(jid);
        }
        self.avatar_queue.truncate(MAX_AVATAR_QUEUE);
        self.pump_avatars()
    }

    pub(super) fn pump_avatars(&mut self) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        while self.avatar_inflight.len() < MAX_AVATAR_FETCHES {
            let Some(jid) = self.avatar_queue.pop_front() else {
                break;
            };
            self.avatar_inflight.insert(jid.clone());
            let (client, dir) = (client.clone(), self.avatar_dir.clone());
            tasks.push(Task::perform(
                async move {
                    let r = fetch_avatar(client, dir, &jid).await;
                    (jid, r)
                },
                |(jid, result)| Message::done(Done::Avatar { jid, result }),
            ));
        }
        Task::batch(tasks)
    }

    pub(super) fn on_avatar(
        &mut self,
        jid: String,
        result: Result<Option<avatar::Tile>, String>,
    ) -> Task<Message> {
        self.avatar_inflight.remove(&jid);
        match result {
            Ok(Some(t)) => {
                self.avatars.put(
                    jid,
                    iced::widget::image::Handle::from_rgba(t.side, t.side, t.rgba),
                    avatar::TILE_BYTES,
                );
            }
            Ok(None) => {
                if self.avatar_none.len() > 5000 {
                    self.avatar_none.clear();
                }
                self.avatar_none.insert(jid);
            }
            Err(e) => tracing::debug!(error = %e, "profile picture unavailable"),
        }
        self.pump_avatars()
    }

    pub(super) fn on_picture_changed(&mut self, jid: String) -> Task<Message> {
        self.avatars.pop(&jid);
        self.avatar_none.remove(&jid);
        if let Some(dir) = self.avatar_dir.clone() {
            let j = jid.clone();
            let _ = blocking(
                move || avatar::invalidate(&dir, &j),
                |()| Message::done(Done::Nothing),
            );
        }
        let visible = self.visible_avatar_jids().contains(&jid);
        if visible {
            self.want_avatars(vec![jid])
        } else {
            Task::none()
        }
    }

    // ------------------------------------------------- typing and presence

    pub(super) fn on_chat_state(
        &mut self,
        chat: String,
        participant: Option<String>,
        state: ChatState,
    ) {
        let chat = self.lid_map.normalize(&chat);
        let participant = participant.map(|p| self.lid_map.normalize(&p));
        if participant.as_deref() == self.own_jid.as_deref() && participant.is_some() {
            return;
        }
        self.typing
            .set(&chat, participant.as_deref(), state, Instant::now());
    }

    pub(super) fn on_live_tick(&mut self) -> Task<Message> {
        let now = Instant::now();
        self.typing.expire(now);
        if self
            .highlight
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= HIGHLIGHT_FOR)
        {
            self.highlight = None;
        }
        match self.compose.on_tick(now) {
            Some(e) => self.emit_chat_state(e),
            None => Task::none(),
        }
    }

    pub(super) fn emit_chat_state(&self, e: live::Emit) -> Task<Message> {
        self.client_task(move |c| async move {
            let _ = c.send_chat_state(&e.chat, e.state).await;
        })
    }

    /// Composer text changed: throttled "typing…" for the open chat.
    pub(super) fn compose_input(&mut self) -> Task<Message> {
        let Some(chat) = self.open_chat_id() else {
            return Task::none();
        };
        if self.editing.is_some() {
            return Task::none();
        }
        let non_empty = !self.composer.is_blank();
        match self.compose.on_input(&chat, non_empty, Instant::now()) {
            Some(e) => self.emit_chat_state(e),
            None => Task::none(),
        }
    }

    /// Stops our "typing…" (sent, chat closed, recording started).
    pub(super) fn compose_stop(&mut self) -> Task<Message> {
        match self.compose.stop() {
            Some(e) => self.emit_chat_state(e),
            None => Task::none(),
        }
    }

    /// Asks the server for a DM contact's online / last seen state once per
    /// session, like the official client does when a chat is opened.
    pub(super) fn subscribe_presence(&mut self, chat: &str) -> Task<Message> {
        if net::is_group_jid(chat) || self.subscribed.contains(chat) || self.client.is_none() {
            return Task::none();
        }
        self.subscribed.insert(chat.to_string());
        let jid = chat.to_string();
        self.client_task(move |c| async move {
            let _ = c.subscribe_presence(&jid).await;
        })
    }

    pub(super) fn reset_live_state(&mut self) {
        self.typing.clear();
        self.compose.stop();
        self.presence.clear();
        self.subscribed.clear();
        self.avatar_inflight.clear();
        self.avatar_queue.clear();
        self.group_sizes.clear();
    }

    // ------------------------------------------------------ group receipts

    /// A group participant's delivered / read receipt: stored per
    /// participant, ticks follow the aggregate (blue only once everyone read).
    pub(super) fn group_receipt(
        &mut self,
        chat: String,
        ids: Vec<String>,
        status: ReceiptStatus,
        participant: String,
    ) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let known = self.group_sizes.get(&chat).copied();
        let store = self.store.clone();
        let participant = self.lid_map.normalize(&participant);
        Task::perform(
            async move {
                let size = match known {
                    Some(s) => Some(s),
                    None => client.group_size(&chat).await.ok(),
                };
                let (c2, p2) = (chat.clone(), participant);
                let changed = tokio::task::spawn_blocking(move || {
                    store
                        .apply_group_receipt(&c2, &ids, Some((&p2, status)), size)
                        .unwrap_or_default()
                })
                .await
                .unwrap_or_default();
                (chat, size, changed)
            },
            |(chat, size, changed)| {
                Message::done(Done::GroupStatus {
                    chat,
                    size,
                    changed,
                })
            },
        )
    }

    pub(super) fn on_group_status(
        &mut self,
        chat: String,
        size: Option<usize>,
        changed: Vec<(String, ReceiptStatus)>,
    ) {
        if let Some(s) = size {
            if self.group_sizes.len() > 500 {
                self.group_sizes.clear();
            }
            self.group_sizes.insert(chat.clone(), s);
        }
        for (id, status) in changed {
            if let Some(c) = &mut self.open
                && c.chat == chat
            {
                c.set_status(&id, status);
            }
            self.chats.set_status(&chat, &id, status);
        }
    }

    // --------------------------------------------------------------- video

    pub(super) fn open_video_external(&mut self, id: &str) -> Task<Message> {
        let Some(m) = self.find_message(id).cloned() else {
            return Task::none();
        };
        let origin = self.media_origin(&m);
        let Content::Video {
            media: Some(mref), ..
        } = m.content
        else {
            return Task::none();
        };
        if self.cfg.mock {
            self.toast("Opening other apps is disabled in mock mode.");
            return Task::none();
        }
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        let dir = self.video_dir();
        let ext = video_extension(&mref.mimetype);
        let name = format!("{}.{ext}", avatar::name_hash(&mref.cache_key()));
        self.toast("Downloading video…");
        Task::perform(
            async move {
                let path = dir.join(&name);
                if !path.is_file() {
                    let bytes = super::media_fetch::download(&client, &mref, Some(&origin)).await?;
                    let (d, p) = (dir.clone(), path.clone());
                    tokio::task::spawn_blocking(move || {
                        crate::config::secure_dir(&d).map_err(|e| e.to_string())?;
                        std::fs::write(&p, &bytes).map_err(|e| e.to_string())?;
                        avatar::prune(&d, MAX_VIDEO_FILES, 400 * 1024 * 1024);
                        Ok::<_, String>(())
                    })
                    .await
                    .map_err(|e| e.to_string())??;
                }
                let p = path.clone();
                tokio::task::spawn_blocking(move || {
                    crate::notify::open_path(&p).map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())??;
                Ok(path)
            },
            |r| Message::done(Done::VideoOpened(r)),
        )
    }

    // ------------------------------------------------------ deferred events

    /// Reactions and edits refer to messages that may still be on their way
    /// into the cache: they wait until the pending writes finished.
    pub(super) fn defer_if_inserting(&mut self, ev: Event) -> Result<(), Event> {
        if self.pending_inserts == 0 {
            return Err(ev);
        }
        if self.deferred.len() >= MAX_DEFERRED {
            self.deferred.remove(0);
        }
        self.deferred.push(ev);
        Ok(())
    }

    pub(super) fn replay_deferred(&mut self) -> Task<Message> {
        if self.pending_inserts > 0 || self.deferred.is_empty() {
            return Task::none();
        }
        let evs: Vec<Event> = std::mem::take(&mut self.deferred);
        Task::batch(evs.into_iter().map(|e| self.on_net(e)))
    }

    // ------------------------------------------------------- net handlers

    pub(super) fn on_reaction_event(
        &mut self,
        chat: String,
        target: String,
        reactor: String,
        emoji: String,
        ts: i64,
    ) -> Task<Message> {
        let chat = self.lid_map.normalize(&chat);
        let reactor = self.lid_map.normalize(&reactor);
        if let Some(c) = &mut self.open
            && c.chat == chat
        {
            c.apply_reaction(&target, &reactor, &emoji, ts);
        }
        store_task(&self.store, move |s| {
            s.apply_reaction(&chat, &target, &reactor, &emoji, ts)
                .map(|_| ())
        })
    }

    pub(super) fn on_edit_event(
        &mut self,
        chat: String,
        id: String,
        sender: String,
        from_me: bool,
        content: Content,
    ) -> Task<Message> {
        let chat = self.lid_map.normalize(&chat);
        let sender = self.lid_map.normalize(&sender);
        if let Some(c) = &mut self.open
            && c.chat == chat
        {
            c.apply_edit(&id, &sender, from_me, &content);
        }
        self.chats
            .apply_edit(&chat, &id, &sender, from_me, &content);
        store_task(&self.store, move |s| {
            s.apply_edit(&chat, &id, &sender, from_me, &content)
                .map(|_| ())
        })
    }

    pub(super) fn on_deleted_for_me(&mut self, chat: String, id: String) -> Task<Message> {
        let chat = self.lid_map.normalize(&chat);
        self.remove_locally(&chat, &id)
    }

    /// Makes the dialog's message id available to views.
    pub fn dialog_message(&self) -> Option<&ChatMessage> {
        match &self.dialog {
            Some(Dialog::Delete(id) | Dialog::Reactions(id)) => self.find_message(id),
            None => None,
        }
    }

    /// The key of a message for protocol actions (tests).
    pub fn key_of(&self, id: &str) -> Option<MsgKey> {
        self.find_message(id).map(|m| m.key())
    }
}

/// Safe file extension for a video MIME type.
pub fn video_extension(mime: &str) -> &'static str {
    let m = mime.split(';').next().unwrap_or_default().trim();
    match m {
        "video/webm" => "webm",
        "video/3gpp" => "3gp",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        _ => "mp4",
    }
}

async fn fetch_avatar(
    client: ClientHandle,
    dir: Option<std::path::PathBuf>,
    jid: &str,
) -> Result<Option<avatar::Tile>, String> {
    let decode = |bytes: Vec<u8>| async move {
        tokio::task::spawn_blocking(move || avatar::prepare(&bytes).map_err(|e| e.to_string()))
            .await
            .map_err(|e| e.to_string())?
            .map(Some)
    };
    if let Some(dir) = &dir {
        let (d, j) = (dir.clone(), jid.to_string());
        let cached = tokio::task::spawn_blocking(move || avatar::read(&d, &j, SystemTime::now()))
            .await
            .map_err(|e| e.to_string())?;
        match cached {
            avatar::Cached::Picture(b) => {
                if let Ok(t) = decode(b).await {
                    return Ok(t);
                }
            }
            avatar::Cached::NoPicture => return Ok(None),
            avatar::Cached::Miss => {}
        }
    }
    let bytes = client
        .profile_picture(jid)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(dir) = dir {
        let (j, b) = (jid.to_string(), bytes.clone());
        let _ = tokio::task::spawn_blocking(move || {
            let r = avatar::write(&dir, &j, b.as_deref());
            avatar::prune(&dir, avatar::MAX_DISK_FILES, avatar::MAX_DISK_BYTES);
            r
        })
        .await;
    }
    match bytes {
        Some(b) => decode(b).await,
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_offset_centres_and_clamps() {
        let l = |top, content| Located {
            top,
            height: 40.0,
            viewport: 400.0,
            content,
            screen_top: 0.0,
        };
        // Middle of a long conversation: top = 1000 + 20 - 200 = 820 from the top;
        // distance from the bottom end = (2000-400) - 820.
        assert_eq!(jump_offset(l(1000.0, 2000.0)), 780.0);
        // Oldest message: clamped to the top => offset is the maximum.
        assert_eq!(jump_offset(l(0.0, 2000.0)), 1600.0);
        // Newest message: clamped to the bottom => offset 0.
        assert_eq!(jump_offset(l(1960.0, 2000.0)), 0.0);
        // Content shorter than the viewport never scrolls.
        assert_eq!(jump_offset(l(100.0, 300.0)), 0.0);
    }

    #[test]
    fn popups_flip_above_when_there_is_no_room_below() {
        let pos = |top| PopupPos {
            top,
            height: 40.0,
            viewport: 600.0,
        };
        // Room below: card sits under the row.
        assert_eq!(popup_top(pos(100.0), 200.0), 144.0);
        // Near the bottom: flips above the row.
        assert_eq!(popup_top(pos(500.0), 200.0), 296.0);
        // Row scrolled partly off the top: still fully inside the viewport.
        assert!(popup_top(pos(-30.0), 200.0) >= 4.0);
        // Card taller than the viewport never goes negative.
        assert_eq!(popup_top(pos(10.0), 700.0), 4.0);
    }

    #[test]
    fn video_extensions_are_allowlisted() {
        assert_eq!(video_extension("video/mp4"), "mp4");
        assert_eq!(video_extension("video/webm; codecs=vp9"), "webm");
        assert_eq!(video_extension("../../etc/passwd"), "mp4");
        assert_eq!(video_extension(""), "mp4");
    }

    #[test]
    fn row_ids_are_stable() {
        assert_eq!(row_id("ABC"), row_id("ABC"));
        assert_ne!(row_id("ABC"), row_id("ABD"));
    }
}
