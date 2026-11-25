//! The contact / group info drawer on the right of a conversation, like
//! WhatsApp Desktop's (a click on the conversation header opens it).
//!
//! Contact: picture, name, phone number, "About" (usync query, the same
//! the official clients send), business name, media / links / docs with a
//! gallery, starred messages of the chat, mute and block, groups in common.
//! Group: subject, member count, description and members with admin badges
//! (group metadata query). Everything local (counts, gallery, stars) comes
//! from the cache; the gallery's thumbnail handles are made in `update`.

use iced::Task;
use iced::widget::image;

use super::App;
use super::message::{Done, Message, Ui};
use crate::net::{self, ContactInfo, Content, GroupInfo, Message as ChatMessage};

/// Most gallery entries loaded per tab.
pub const GALLERY_LIMIT: usize = 120;
/// Starred messages shown in the drawer.
pub const STARRED_SHOWN: usize = 3;

/// A value that is fetched from the phone / server.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Load<T> {
    /// Asked, no answer yet.
    #[default]
    Loading,
    /// Answer.
    Ready(T),
    /// Failed or not available (privacy, not a member, offline).
    Unavailable,
}

impl<T> Load<T> {
    /// The value, if it arrived.
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(v) => Some(v),
            _ => None,
        }
    }

    fn from_result<E: std::fmt::Display>(r: Result<T, E>, what: &str) -> Self {
        match r {
            Ok(v) => Load::Ready(v),
            Err(e) => {
                tracing::warn!(error = %e, what, "info drawer query failed");
                Load::Unavailable
            }
        }
    }
}

/// Which list the drawer's gallery shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Photos and videos.
    Media,
    /// Documents.
    Docs,
    /// Link previews.
    Links,
}

impl Tab {
    fn kinds(self) -> &'static [&'static str] {
        match self {
            Tab::Media => &["image", "video"],
            Tab::Docs => &["document"],
            Tab::Links => &["link"],
        }
    }
}

/// One gallery entry.
#[derive(Debug, Clone)]
pub struct Item {
    /// The message.
    pub message: ChatMessage,
    /// Its thumbnail (photos, videos, link previews).
    pub thumb: Option<image::Handle>,
}

/// Results of the drawer's queries.
#[derive(Debug, Clone)]
pub enum InfoDone {
    /// About / business name.
    Contact(String, Result<ContactInfo, String>),
    /// Group metadata.
    Group(String, Result<GroupInfo, String>),
    /// Groups in common.
    Common(String, Result<Vec<(String, String)>, String>),
    /// Block state.
    Blocked(String, Result<bool, String>),
    /// Local counts, the first gallery row and starred messages.
    Local {
        /// Chat.
        chat: String,
        /// `(media, links, docs)`.
        counts: (usize, usize, usize),
        /// Newest photos / videos.
        recent: Vec<ChatMessage>,
        /// Starred messages of this chat, newest star first.
        starred: Vec<ChatMessage>,
    },
    /// A gallery tab's list.
    Gallery(String, Tab, Vec<ChatMessage>),
    /// Block / unblock finished.
    BlockSet(String, bool, Result<(), String>),
}

/// State of the open drawer.
#[derive(Debug, Clone)]
pub struct InfoPanel {
    /// Chat it describes.
    pub chat: String,
    /// About / business (contacts).
    pub contact: Load<ContactInfo>,
    /// Metadata (groups).
    pub group: Load<GroupInfo>,
    /// Groups in common (contacts).
    pub common: Load<Vec<(String, String)>>,
    /// On our block list (contacts).
    pub blocked: Load<bool>,
    /// `(media, links, docs)`.
    pub counts: (usize, usize, usize),
    /// The newest photos / videos (drawer row).
    pub recent: Vec<Item>,
    /// Starred messages of this chat.
    pub starred: Vec<ChatMessage>,
    /// The gallery page, when open.
    pub gallery: Option<(Tab, Vec<Item>)>,
    /// "Block X?" confirmation shown.
    pub confirm_block: bool,
}

impl InfoPanel {
    /// A drawer for `chat`, everything loading.
    pub fn new(chat: String) -> Self {
        Self {
            chat,
            contact: Load::Loading,
            group: Load::Loading,
            common: Load::Loading,
            blocked: Load::Loading,
            counts: (0, 0, 0),
            recent: Vec::new(),
            starred: Vec::new(),
            gallery: None,
            confirm_block: false,
        }
    }
}

/// The thumbnail bytes a message carries.
fn thumb_of(m: &ChatMessage) -> Option<&Vec<u8>> {
    match &m.content {
        Content::Image { thumbnail, .. } | Content::Video { thumbnail, .. } => thumbnail.as_ref(),
        Content::Link { preview, .. } => preview.thumbnail.as_ref(),
        _ => None,
    }
}

/// Gallery entries with their handles (made here, in `update`).
fn items(msgs: Vec<ChatMessage>) -> Vec<Item> {
    msgs.into_iter()
        .map(|m| {
            let thumb = thumb_of(&m).map(|t| image::Handle::from_bytes(t.clone()));
            Item { message: m, thumb }
        })
        .collect()
}

/// The ids a person is known by (phone number and LID), for "groups in
/// common" and blocking.
pub fn person_ids(jid: &str, alias: Option<&str>) -> Vec<String> {
    let mut v = vec![jid.to_string()];
    if let Some(a) = alias.filter(|a| *a != jid) {
        v.push(a.to_string());
    }
    v
}

impl App {
    /// Header click: open the drawer for the open chat (or close it).
    pub(super) fn toggle_info(&mut self) -> Task<Message> {
        let Some(chat) = self.open.as_ref().map(|c| c.chat.clone()) else {
            return Task::none();
        };
        if self.info.as_ref().is_some_and(|i| i.chat == chat) {
            self.info = None;
            return Task::none();
        }
        self.popup = None;
        self.msg_info = None;
        self.info = Some(InfoPanel::new(chat.clone()));
        let group = net::is_group_jid(&chat);
        let done = |d: InfoDone| Message::done(Done::Info(Box::new(d)));
        let mut tasks = Vec::new();
        let local = {
            let chat = chat.clone();
            let s = self.store.clone();
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let counts = s.media_counts(&chat).unwrap_or_default();
                        let recent = s
                            .messages_of_kind(&chat, Tab::Media.kinds(), 4)
                            .unwrap_or_default();
                        let starred = s
                            .starred_messages(crate::store::MAX_STARRED)
                            .unwrap_or_default()
                            .into_iter()
                            .map(|st| st.message)
                            .filter(|m| m.chat == chat)
                            .collect();
                        InfoDone::Local {
                            chat,
                            counts,
                            recent,
                            starred,
                        }
                    })
                    .await
                    .ok()
                },
                move |r| match r {
                    Some(d) => done(d),
                    None => Message::done(Done::Nothing),
                },
            )
        };
        tasks.push(local);
        let Some(client) = self.client.clone() else {
            if let Some(i) = &mut self.info {
                i.contact = Load::Unavailable;
                i.group = Load::Unavailable;
                i.common = Load::Unavailable;
                i.blocked = Load::Unavailable;
            }
            return Task::batch(tasks);
        };
        if group {
            let c = client.clone();
            let ch = chat.clone();
            tasks.push(Task::perform(
                async move {
                    let r = c.group_info(&ch).await.map_err(|e| e.to_string());
                    (ch, r)
                },
                move |(ch, r)| done(InfoDone::Group(ch, r)),
            ));
        } else {
            let ids = person_ids(&chat, self.chats.names.alias_of(&chat));
            let c = client.clone();
            let ch = chat.clone();
            tasks.push(Task::perform(
                async move {
                    let r = c.contact_info(&ch).await.map_err(|e| e.to_string());
                    (ch, r)
                },
                move |(ch, r)| done(InfoDone::Contact(ch, r)),
            ));
            let c = client.clone();
            let ch = chat.clone();
            tasks.push(Task::perform(
                async move {
                    let r = c.common_groups(&ids).await.map_err(|e| e.to_string());
                    (ch, r)
                },
                move |(ch, r)| done(InfoDone::Common(ch, r)),
            ));
            let c = client;
            let ch = chat;
            tasks.push(Task::perform(
                async move {
                    let r = c.is_blocked(&ch).await.map_err(|e| e.to_string());
                    (ch, r)
                },
                move |(ch, r)| done(InfoDone::Blocked(ch, r)),
            ));
        }
        Task::batch(tasks)
    }

    /// A drawer query finished.
    pub(super) fn on_info(&mut self, d: InfoDone) -> Task<Message> {
        let chat = match &d {
            InfoDone::Contact(c, _)
            | InfoDone::Group(c, _)
            | InfoDone::Common(c, _)
            | InfoDone::Blocked(c, _)
            | InfoDone::Gallery(c, _, _)
            | InfoDone::BlockSet(c, _, _) => c.clone(),
            InfoDone::Local { chat, .. } => chat.clone(),
        };
        if let InfoDone::BlockSet(_, blocked, Err(e)) = &d {
            tracing::warn!(error = %e, "block change failed");
            self.toast(if *blocked {
                "Could not block this contact."
            } else {
                "Could not unblock this contact."
            });
        }
        let Some(info) = self.info.as_mut().filter(|i| i.chat == chat) else {
            return Task::none();
        };
        match d {
            InfoDone::Contact(_, r) => info.contact = Load::from_result(r, "contact"),
            InfoDone::Group(_, r) => info.group = Load::from_result(r, "group"),
            InfoDone::Common(_, r) => info.common = Load::from_result(r, "common groups"),
            InfoDone::Blocked(_, r) => info.blocked = Load::from_result(r, "block list"),
            InfoDone::Local {
                counts,
                recent,
                starred,
                ..
            } => {
                info.counts = counts;
                info.recent = items(recent);
                info.starred = starred;
            }
            InfoDone::Gallery(_, tab, msgs) => info.gallery = Some((tab, items(msgs))),
            InfoDone::BlockSet(_, blocked, r) => {
                if r.is_ok() {
                    info.blocked = Load::Ready(blocked);
                }
            }
        }
        Task::none()
    }

    /// Drawer actions; `Err(ui)` for everything else.
    pub(super) fn on_ui_info(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        Ok(match ui {
            Ui::ToggleInfo => self.toggle_info(),
            Ui::CloseInfo => {
                self.info = None;
                Task::none()
            }
            Ui::InfoGallery(tab) => {
                let Some(info) = self.info.as_mut() else {
                    return Ok(Task::none());
                };
                let Some(tab) = tab else {
                    info.gallery = None;
                    return Ok(Task::none());
                };
                info.gallery = Some((tab, Vec::new()));
                let chat = info.chat.clone();
                let s = self.store.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let msgs = s
                                .messages_of_kind(&chat, tab.kinds(), GALLERY_LIMIT)
                                .unwrap_or_default();
                            InfoDone::Gallery(chat, tab, msgs)
                        })
                        .await
                        .ok()
                    },
                    |r| match r {
                        Some(d) => Message::done(Done::Info(Box::new(d))),
                        None => Message::done(Done::Nothing),
                    },
                )
            }
            Ui::ToggleMute(chat) => {
                // Unmutes at once; muting asks for how long, like the
                // official client.
                if self.is_muted(&chat) {
                    self.set_mute(chat, crate::net::MUTE_OFF)
                } else {
                    return self.on_ui_chat_actions(Ui::ChatAct(
                        super::chat_actions::ChatAct::AskMute(chat),
                    ));
                }
            }
            Ui::AskBlock => {
                if let Some(i) = &mut self.info {
                    i.confirm_block = true;
                }
                Task::none()
            }
            Ui::CancelBlock => {
                if let Some(i) = &mut self.info {
                    i.confirm_block = false;
                }
                Task::none()
            }
            Ui::SetBlocked(blocked) => {
                let Some(info) = self.info.as_mut() else {
                    return Ok(Task::none());
                };
                info.confirm_block = false;
                let chat = info.chat.clone();
                let Some(client) = self.client.clone() else {
                    return Ok(Task::none());
                };
                Task::perform(
                    async move {
                        let r = client
                            .set_blocked(&chat, blocked)
                            .await
                            .map_err(|e| e.to_string());
                        (chat, r)
                    },
                    move |(chat, r)| {
                        Message::done(Done::Info(Box::new(InfoDone::BlockSet(chat, blocked, r))))
                    },
                )
            }
            other => return Err(other),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_states() {
        let ok: Load<u8> = Load::from_result(Ok::<u8, String>(3), "x");
        assert_eq!(ok.ready(), Some(&3));
        let bad: Load<u8> = Load::from_result(Err::<u8, String>("no".into()), "x");
        assert_eq!(bad, Load::Unavailable);
        assert_eq!(Load::<u8>::default(), Load::Loading);
    }

    #[test]
    fn person_ids_include_the_other_id_once() {
        assert_eq!(
            person_ids("1@s.whatsapp.net", Some("9@lid")),
            vec!["1@s.whatsapp.net".to_string(), "9@lid".to_string()]
        );
        assert_eq!(person_ids("1@s.whatsapp.net", None).len(), 1);
        assert_eq!(
            person_ids("1@s.whatsapp.net", Some("1@s.whatsapp.net")).len(),
            1
        );
    }
}
