//! Archive, pin, favourites, custom lists and links: user actions that
//! change how the chat list is organised.
//!
//! Every change is applied locally first (the list updates at once), then
//! written to the cache and sent to the phone through app-state sync, the
//! way the official client does. A failed send is reverted with a toast.

use iced::Task;
use iced::widget::operation;

use super::message::{Done, Message, Ui};
use super::state::{Filter, LOCAL_FAVOURITES};
use super::{App, ListDialog, MenuTarget, SidebarMenu, blocking, input, linkify, store_task};
use crate::net::{ChatFlag, ListDef, ListKind};

/// Id of the list name input in the list dialog.
pub const LIST_NAME_ID: &str = "list-name";
/// Longest list name (the official client limits names to 30 characters).
pub const MAX_LIST_NAME: usize = 30;
/// Most chats that can be pinned (WhatsApp's limit).
pub const MAX_PINNED: usize = 3;
/// Number of colour indexes WhatsApp offers for lists.
const LIST_COLORS: i32 = 20;

/// Result of validating a list name typed by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameCheck {
    /// Usable (trimmed).
    Ok(String),
    /// Empty after trimming.
    Empty,
    /// Another list already has this name.
    Duplicate,
}

/// Validates a list name against the existing lists (`editing` is the id
/// being renamed, which may keep its own name).
pub fn check_list_name(name: &str, lists: &[ListDef], editing: Option<&str>) -> NameCheck {
    let n: String = name.trim().chars().take(MAX_LIST_NAME).collect();
    if n.is_empty() {
        return NameCheck::Empty;
    }
    let dup = lists.iter().any(|l| {
        l.kind == ListKind::Custom
            && Some(l.id.as_str()) != editing
            && l.name.to_lowercase() == n.to_lowercase()
    });
    if dup {
        NameCheck::Duplicate
    } else {
        NameCheck::Ok(n)
    }
}

impl App {
    /// Handles the list / archive / link messages; `Err(ui)` for the rest.
    pub(super) fn on_ui_lists(&mut self, ui: Ui) -> Result<Task<Message>, Ui> {
        Ok(match ui {
            Ui::OpenArchived => {
                self.archived_view = true;
                self.filter = Filter::All;
                self.sidebar_menu = None;
                self.want_visible_avatars()
            }
            Ui::CloseArchived => {
                self.archived_view = false;
                self.want_visible_avatars()
            }
            Ui::ChatMenu(chat) => {
                self.sidebar_menu = Some(SidebarMenu {
                    target: MenuTarget::Chat(chat),
                    at: input::get(),
                });
                Task::none()
            }
            Ui::ChipMenu(id) => {
                self.sidebar_menu = Some(SidebarMenu {
                    target: MenuTarget::Chip(id),
                    at: input::get(),
                });
                Task::none()
            }
            Ui::CloseSidebarMenu => {
                self.sidebar_menu = None;
                Task::none()
            }
            Ui::ToggleArchive(chat) => {
                self.sidebar_menu = None;
                let now = self.chats.get(&chat).is_some_and(|e| e.meta.archived);
                self.set_flag(chat, ChatFlag::Archived(!now), ChatFlag::Archived(now))
            }
            Ui::TogglePin(chat) => {
                self.sidebar_menu = None;
                let now = self.chats.get(&chat).is_some_and(|e| e.meta.pinned);
                if !now && self.chats.pinned_count() >= MAX_PINNED {
                    self.toast(format!("You can only pin up to {MAX_PINNED} chats."));
                    return Ok(Task::none());
                }
                self.set_flag(chat, ChatFlag::Pinned(!now), ChatFlag::Pinned(now))
            }
            Ui::ToggleFavourite(chat) => {
                self.sidebar_menu = None;
                self.toggle_favourite(chat)
            }
            Ui::ToggleInList { chat, list } => {
                self.sidebar_menu = None;
                let member = !self.chats.in_list(&list, &chat);
                self.set_member(&list, chat, member, true)
            }
            Ui::NewList => {
                self.list_dialog = Some(ListDialog::Edit {
                    id: None,
                    name: String::new(),
                });
                operation::focus(LIST_NAME_ID)
            }
            Ui::RenameList(id) => {
                self.sidebar_menu = None;
                let name = self
                    .chats
                    .list(&id)
                    .map(|l| l.name.clone())
                    .unwrap_or_default();
                self.list_dialog = Some(ListDialog::Edit { id: Some(id), name });
                operation::focus(LIST_NAME_ID)
            }
            Ui::ListName(n) => {
                if let Some(ListDialog::Edit { name, .. }) = &mut self.list_dialog {
                    *name = n.chars().take(MAX_LIST_NAME).collect();
                }
                Task::none()
            }
            Ui::SaveList => self.save_list_dialog(),
            Ui::AskDeleteList(id) => {
                self.sidebar_menu = None;
                self.list_dialog = Some(ListDialog::Delete(id));
                Task::none()
            }
            Ui::ConfirmDeleteList(id) => {
                self.list_dialog = None;
                self.delete_list(id)
            }
            Ui::CloseListDialog => {
                self.list_dialog = None;
                Task::none()
            }
            Ui::OpenLink(href) => self.open_link(href),
            other => return self.on_ui_chat_actions(other),
        })
    }

    /// Optimistic archive / pin change, sent to the phone; `undo` is applied
    /// again if the send fails.
    fn set_flag(&mut self, chat: String, flag: ChatFlag, undo: ChatFlag) -> Task<Message> {
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
                let r = match flag {
                    ChatFlag::Archived(v) => client.set_archived(&chat, v).await,
                    ChatFlag::Pinned(v) => client.set_pinned(&chat, v).await,
                    ChatFlag::Muted(_) => Ok(()),
                };
                (chat, r.map_err(|e| e.to_string()))
            },
            move |(chat, r)| match r {
                Ok(()) => Message::done(Done::Nothing),
                Err(error) => Message::done(Done::FlagFailed { chat, undo, error }),
            },
        );
        Task::batch([persist, send])
    }

    pub(super) fn on_flag_failed(
        &mut self,
        chat: String,
        undo: ChatFlag,
        error: String,
    ) -> Task<Message> {
        tracing::warn!(error = %error, "chat flag sync failed");
        self.toast("Could not sync this change with your phone.");
        self.chats.apply_flag(&chat, undo);
        store_task(&self.store, move |s| s.set_chat_flag(&chat, undo))
    }

    fn toggle_favourite(&mut self, chat: String) -> Task<Message> {
        let fav = self.chats.is_favourite(&chat);
        let target = self.chats.favourites_target().to_string();
        if fav {
            // Remove from every place it is kept.
            let mut tasks = Vec::new();
            for list in [LOCAL_FAVOURITES.to_string(), target] {
                if self.chats.in_list(&list, &chat) {
                    tasks.push(self.set_member(&list, chat.clone(), false, true));
                }
            }
            Task::batch(tasks)
        } else {
            self.set_member(&target, chat, true, true)
        }
    }

    /// Membership change: local state, cache and (for synced lists) phone.
    fn set_member(&mut self, list: &str, chat: String, member: bool, send: bool) -> Task<Message> {
        self.chats.apply_list_chat(list, &chat, member);
        let persist = {
            let (l, c) = (list.to_string(), chat.clone());
            store_task(&self.store, move |s| s.set_list_member(&l, &c, member))
        };
        let (Some(client), true) = (self.client.clone(), send && list != LOCAL_FAVOURITES) else {
            return persist;
        };
        let list = list.to_string();
        let sync = Task::perform(
            async move {
                let r = client.set_list_member(&list, &chat, member).await;
                (list, chat, r.map_err(|e| e.to_string()))
            },
            move |(list, chat, r)| match r {
                Ok(()) => Message::done(Done::Nothing),
                Err(error) => Message::done(Done::MemberFailed {
                    list,
                    chat,
                    undo: !member,
                    error,
                }),
            },
        );
        Task::batch([persist, sync])
    }

    pub(super) fn on_member_failed(
        &mut self,
        list: String,
        chat: String,
        undo: bool,
        error: String,
    ) -> Task<Message> {
        tracing::warn!(error = %error, "list sync failed");
        self.toast("Could not sync this change with your phone.");
        self.set_member(&list, chat, undo, false)
    }

    fn save_list_dialog(&mut self) -> Task<Message> {
        let Some(ListDialog::Edit { id, name }) = self.list_dialog.clone() else {
            return Task::none();
        };
        let lists: Vec<ListDef> = self.chats.lists.clone();
        let name = match check_list_name(&name, &lists, id.as_deref()) {
            NameCheck::Ok(n) => n,
            NameCheck::Empty => return Task::none(),
            NameCheck::Duplicate => {
                self.toast("A list with this name already exists.");
                return Task::none();
            }
        };
        self.list_dialog = None;
        let def = match id.as_deref().and_then(|i| self.chats.list(i)).cloned() {
            Some(old) => ListDef { name, ..old },
            None => ListDef {
                id: self.chats.next_list_id(),
                color: self.chats.custom_lists().count() as i32 % LIST_COLORS,
                kind: ListKind::Custom,
                order: self.chats.next_list_order(),
                name,
            },
        };
        let new = id.is_none();
        self.chats.apply_list_edit(def.clone(), false);
        if new {
            self.filter = Filter::List(def.id.clone());
            self.archived_view = false;
        }
        let persist = {
            let d = def.clone();
            store_task(&self.store, move |s| s.save_list(&d))
        };
        let Some(client) = self.client.clone() else {
            return persist;
        };
        let send = Task::perform(
            async move { client.save_list(&def).await.map_err(|e| e.to_string()) },
            |r| match r {
                Ok(()) => Message::done(Done::Nothing),
                Err(e) => Message::done(Done::ActionDone {
                    what: "save the list",
                    result: Err(e),
                }),
            },
        );
        Task::batch([persist, send])
    }

    fn delete_list(&mut self, id: String) -> Task<Message> {
        let Some(def) = self.chats.list(&id).cloned() else {
            return Task::none();
        };
        self.chats.apply_list_edit(def, true);
        if self.filter == Filter::List(id.clone()) {
            self.filter = Filter::All;
        }
        let persist = {
            let id = id.clone();
            store_task(&self.store, move |s| s.delete_list(&id))
        };
        let Some(client) = self.client.clone() else {
            return persist;
        };
        let send = Task::perform(
            async move { client.delete_list(&id).await.map_err(|e| e.to_string()) },
            |r| match r {
                Ok(()) => Message::done(Done::Nothing),
                Err(e) => Message::done(Done::ActionDone {
                    what: "delete the list",
                    result: Err(e),
                }),
            },
        );
        Task::batch([persist, send])
    }

    /// Opens a link from a message in the default browser (`xdg-open`),
    /// only for http(s) and mailto targets.
    fn open_link(&mut self, href: String) -> Task<Message> {
        if !linkify::is_safe_href(&href) {
            self.toast("This link cannot be opened.");
            return Task::none();
        }
        blocking(
            move || open::that_detached(&href).map_err(|e| e.to_string()),
            |r| match r {
                Ok(()) => Message::done(Done::Nothing),
                Err(e) => Message::done(Done::ActionDone {
                    what: "open the link",
                    result: Err(e),
                }),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(id: &str, name: &str) -> ListDef {
        ListDef {
            id: id.into(),
            name: name.into(),
            kind: ListKind::Custom,
            ..Default::default()
        }
    }

    #[test]
    fn list_names_are_validated() {
        let lists = vec![custom("1", "Family"), custom("2", "Work")];
        assert_eq!(check_list_name("  ", &lists, None), NameCheck::Empty);
        assert_eq!(
            check_list_name("family", &lists, None),
            NameCheck::Duplicate
        );
        // Renaming a list to its own name is fine.
        assert_eq!(
            check_list_name("Family", &lists, Some("1")),
            NameCheck::Ok("Family".into())
        );
        assert_eq!(
            check_list_name(" Gym ", &lists, None),
            NameCheck::Ok("Gym".into())
        );
        let long = "x".repeat(80);
        assert_eq!(
            check_list_name(&long, &lists, None),
            NameCheck::Ok("x".repeat(MAX_LIST_NAME))
        );
    }
}
