//! Navigation rail and chat list column.

use iced::widget::text::Wrapping;
use iced::widget::{
    Space, button, checkbox, column, container, mouse_area, opaque, radio, row, scrollable, stack,
    text, text_input,
};
use iced::{Alignment, Length};

use super::emoji_font::emoji_text;
use super::icons::Icon;
use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, avatar_for, badge, ellipsize, fill, gap, icon, list_time, ticks};
use crate::app::chat_actions::{ChatAct, ChatDialog, MuteFor};
use crate::app::lists::{LIST_NAME_ID, MAX_LIST_NAME};
use crate::app::nav::{NavMsg, Section};
use crate::app::state::{ChatEntry, Filter};
use crate::app::{App, ListDialog, MenuTarget, Message, Ui};
use crate::net::Connection;

/// Rows rendered at most (the list is not virtualised; search reaches the rest).
pub const MAX_ROWS: usize = 300;

fn section_button(inner: El<'_>, s: Section) -> El<'_> {
    button(inner)
        .on_press(Message::Ui(Ui::Nav(NavMsg::ShowSection(s))))
        .padding(0)
        .style(theme::flat_button(iced::Color::TRANSPARENT))
        .into()
}

/// Left navigation rail: Chats, Status and Communities.
pub fn rail(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let item = |i: Icon, active: bool| -> El<'_> {
        container(icon(
            i,
            22.0,
            if active {
                p.text_primary
            } else {
                p.text_secondary
            },
        ))
        .padding(9)
        .style(if active {
            theme::rounded(p.bg_selected, 20.0)
        } else {
            theme::rounded(iced::Color::TRANSPARENT, 20.0)
        })
        .into()
    };
    let current = if app.settings_open {
        None
    } else {
        Some(app.nav.section)
    };
    let unread = app.unread_chats();
    let chats_active = current == Some(Section::Chats);
    let chats_item: El<'_> = if unread > 0 {
        stack![
            item(Icon::MessageCircle, chats_active),
            container(badge(t, unread, false))
                .width(40)
                .align_x(Alignment::End)
                .padding(iced::Padding {
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                    left: 18.0
                })
        ]
        .into()
    } else {
        item(Icon::MessageCircle, chats_active)
    };
    // A green dot on the Status icon while there are updates not seen yet.
    let status_item: El<'_> = if app.status.has_unseen(
        app.own_jid.as_deref().unwrap_or_default(),
        chrono::Utc::now().timestamp(),
    ) {
        stack![
            item(Icon::CircleDashed, current == Some(Section::Status)),
            container(
                container(Space::new().width(9).height(9)).style(theme::rounded(p.accent, 5.0))
            )
            .width(40)
            .align_x(Alignment::End)
            .padding(iced::Padding {
                top: 6.0,
                right: 6.0,
                bottom: 0.0,
                left: 0.0
            })
        ]
        .into()
    } else {
        item(Icon::CircleDashed, current == Some(Section::Status))
    };
    container(
        column![
            section_button(chats_item, Section::Chats),
            section_button(status_item, Section::Status),
            section_button(
                item(Icon::UsersRound, current == Some(Section::Communities)),
                Section::Communities
            ),
            Space::new().height(Length::Fill),
            button(item(Icon::Settings, app.settings_open))
                .on_press(Message::Ui(if app.settings_open {
                    Ui::CloseSettings
                } else {
                    Ui::OpenSettings
                }))
                .padding(0)
                .style(theme::flat_button(iced::Color::TRANSPARENT)),
            container(avatar_for(
                app,
                app.own_jid.as_deref().unwrap_or_default(),
                false,
                32.0
            ))
            .padding(4),
        ]
        .spacing(10)
        .align_x(Alignment::Center),
    )
    .width(t.metrics.rail_width)
    .height(Length::Fill)
    .padding([12, 0])
    .center_x(t.metrics.rail_width)
    .style(theme::solid(p.bg_rail))
    .into()
}

/// The chat list column.
pub fn view(app: &App) -> El<'_> {
    if let Some(el) = super::nav::sidebar_override(app) {
        return el;
    }
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;

    let header = if app.archived_view {
        row![
            button(icon(Icon::ArrowLeft, m.icon, p.text_secondary))
                .padding(8)
                .style(theme::icon_button(t, false))
                .on_press(Message::Ui(Ui::CloseArchived)),
            text("Archived")
                .size(m.text_title)
                .font(INTER_SEMIBOLD)
                .color(p.text_primary),
            fill(),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .padding(iced::Padding {
            top: 10.0,
            right: 10.0,
            bottom: 6.0,
            left: 12.0,
        })
    } else {
        row![
            text("Chats")
                .size(m.text_title)
                .font(INTER_SEMIBOLD)
                .color(p.text_primary),
            fill(),
            button(icon(Icon::MessageSquarePlus, m.icon, p.text_secondary))
                .padding(8)
                .style(theme::icon_button(t, false))
                .on_press(Message::Ui(Ui::Nav(NavMsg::OpenNewChat))),
            button(icon(Icon::EllipsisVertical, m.icon, p.text_secondary))
                .padding(8)
                .style(theme::icon_button(t, app.menu_open))
                .on_press(Message::Ui(Ui::ToggleMenu)),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .padding(iced::Padding {
            top: 10.0,
            right: 10.0,
            bottom: 6.0,
            left: 20.0,
        })
    };

    let search = container(
        row![
            icon(Icon::Search, 18.0, p.text_secondary),
            text_input("Search or start a new chat", &app.search)
                .on_input(|s| Message::Ui(Ui::Search(s)))
                .size(m.text)
                .padding([6, 0])
                .style(theme::input(t)),
        ]
        .spacing(14)
        .align_y(Alignment::Center),
    )
    .padding([3, 16])
    .style(theme::rounded(p.bg_input, 20.0));

    let mut top = column![header, container(search).padding([4, 14])];
    if !app.archived_view {
        top = top.push(container(chips(app)).padding([10, 16]));
    }

    if app.connection != Connection::Connected {
        let (label, action) = match &app.connection {
            Connection::Reconnecting { attempt } => {
                (format!("Reconnecting… (attempt {attempt})"), None)
            }
            Connection::Connecting => ("Connecting…".into(), None),
            Connection::Replaced => (
                "WhatsApp is open on another computer or browser. Ferrochat is paused.".into(),
                Some("Use here"),
            ),
            Connection::Offline { reason } => (
                format!("{reason} Retrying automatically."),
                Some("Retry now"),
            ),
            Connection::Disconnected | Connection::Connected => {
                ("Computer not connected".into(), Some("Retry now"))
            }
        };
        top = top.push(banner(app, Icon::WifiOff, label, action));
    } else if let Some(pr) = app.sync_progress {
        top = top.push(banner(
            app,
            Icon::RefreshCw,
            format!("Syncing older messages… {pr}%"),
            None,
        ));
    }

    let now = chrono::Utc::now().timestamp();
    let entries = app
        .chats
        .filtered_view(&app.search, &app.filter, app.archived_view);
    let searching = !app.search.trim().is_empty();
    let hits = if searching {
        &app.message_hits[..]
    } else {
        &[]
    };
    let list: El<'_> = if entries.is_empty() && hits.is_empty() {
        container(
            text(if app.archived_view && app.search.is_empty() {
                "No archived chats"
            } else if app.search.is_empty() && app.filter != Filter::All {
                "No chats in this list"
            } else if app.search.is_empty() {
                "No chats yet"
            } else {
                "No chats, contacts or messages found"
            })
            .size(m.text)
            .color(p.text_secondary),
        )
        .center_x(Length::Fill)
        .padding(40)
        .into()
    } else {
        let mut col = column![];
        let archived = app.chats.archived_count();
        if archived > 0 && !app.archived_view && app.filter == Filter::All && app.search.is_empty()
        {
            col = col.push(archived_row(app));
        }
        let total = entries.len();
        for e in entries.into_iter().take(MAX_ROWS) {
            col = col.push(chat_row(app, e, now));
        }
        if total > MAX_ROWS {
            col = col.push(
                container(
                    text(format!(
                        "{} more chats. Search to find older chats.",
                        total - MAX_ROWS
                    ))
                    .size(13)
                    .color(p.text_secondary),
                )
                .center_x(Length::Fill)
                .padding(16),
            );
        }
        if !hits.is_empty() {
            col = col.push(
                container(text("Messages").size(14).font(INTER_MEDIUM).color(p.accent))
                    .padding([14, 20]),
            );
            for h in hits {
                col = col.push(hit_row(app, h, now));
            }
        }
        scrollable(col)
            .direction(super::thin_scrollbar())
            .style(theme::scrollbar(t))
            .on_scroll(|v| {
                Message::Ui(Ui::SidebarScrolled {
                    offset_y: v.absolute_offset().y,
                    height: v.bounds().height,
                })
            })
            .height(Length::Fill)
            .into()
    };

    let body = column![top, list].height(Length::Fill);

    let mut layers = vec![body.into()];
    if app.menu_open {
        layers.push(menu(app));
    }
    container(stack(layers))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::solid(p.bg_sidebar))
        .into()
}

fn banner<'a>(app: &'a App, i: Icon, label: String, action: Option<&'static str>) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let mut r = row![
        container(icon(i, 20.0, p.bg_sidebar))
            .padding(10)
            .style(theme::rounded(p.notice_text, 22.0)),
        text(label)
            .size(14)
            .color(p.text_primary)
            .width(Length::Fill),
    ]
    .spacing(14)
    .align_y(Alignment::Center);
    if let Some(a) = action {
        r = r.push(
            button(text(a).size(14).font(INTER_MEDIUM))
                .padding([6, 4])
                .style(theme::link_button(t))
                .on_press(Message::Ui(Ui::Reconnect)),
        );
    }
    container(r)
        .padding([10, 16])
        .width(Length::Fill)
        .style(theme::solid(p.system_chip))
        .into()
}

fn menu(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let item = |label: &'static str, i: Icon, msg: Option<Message>| -> El<'_> {
        let b = button(
            row![
                icon(i, 18.0, p.text_secondary),
                text(label).size(14.5).color(p.text_primary)
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([10, 20])
        .style(theme::chat_row(t, false));
        match msg {
            Some(m) => b.on_press(m).into(),
            None => b.into(),
        }
    };
    let card = container(
        column![
            item(
                "New group",
                Icon::Users,
                Some(Message::Ui(Ui::Nav(NavMsg::OpenNewGroup)))
            ),
            item(
                "Starred messages",
                Icon::Star,
                Some(Message::Ui(Ui::Nav(NavMsg::OpenStarred)))
            ),
            item(
                "Settings",
                Icon::Settings,
                Some(Message::Ui(Ui::OpenSettings))
            ),
            item(
                "Log out",
                Icon::LogOut,
                Some(Message::Ui(Ui::Nav(NavMsg::AskLogout)))
            ),
        ]
        .width(220),
    )
    .padding([8, 0])
    .style(move |th: &iced::Theme| {
        let mut s = theme::rounded(p.bg_elevated, 6.0)(th);
        s.shadow = iced::Shadow {
            color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.4),
            offset: iced::Vector::new(0.0, 2.0),
            blur_radius: 10.0,
        };
        s
    });
    container(card)
        .width(Length::Fill)
        .align_x(Alignment::End)
        .padding(iced::Padding {
            top: 52.0,
            right: 12.0,
            bottom: 0.0,
            left: 0.0,
        })
        .into()
}

/// A message found by the chat list search: chat name, snippet and time.
fn hit_row<'a>(app: &'a App, h: &'a crate::store::SearchHit, now: i64) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let name = app.chats.display_name(&h.chat);
    let who = if h.from_me {
        "You: ".to_string()
    } else if crate::net::is_group_jid(&h.chat) {
        format!("{}: ", ellipsize(&app.chats.display_name(&h.sender), 14))
    } else {
        String::new()
    };
    let content = column![
        row![
            emoji_text(
                ellipsize(&name, 34),
                16.5,
                p.text_primary,
                theme::INTER,
                false
            ),
            fill(),
            text(list_time(h.ts, now)).size(12).color(p.text_secondary),
        ]
        .align_y(Alignment::Center),
        text(format!("{who}{}", ellipsize(&h.snippet, 60)))
            .size(m.text_small + 0.5)
            .color(p.text_secondary)
            .wrapping(Wrapping::None)
            .shaping(text::Shaping::Advanced),
    ]
    .spacing(3)
    .width(Length::Fill);
    button(
        row![
            container(avatar_for(
                app,
                &h.chat,
                crate::net::is_group_jid(&h.chat),
                m.avatar
            ))
            .padding([0, 15]),
            container(content).padding(iced::Padding {
                right: 15.0,
                ..Default::default()
            }),
        ]
        .align_y(Alignment::Center),
    )
    .padding(0)
    .width(Length::Fill)
    .height(m.row_height)
    .style(theme::chat_row(t, false))
    .on_press(Message::Ui(Ui::OpenHit {
        chat: h.chat.clone(),
        id: h.id.clone(),
    }))
    .into()
}

fn chat_row<'a>(app: &'a App, e: &'a ChatEntry, now: i64) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let selected = app.open.as_ref().is_some_and(|c| c.chat == e.meta.id);
    let name = app.chats.display_name(&e.meta.id);
    let unread = e.meta.unread;

    let time = e
        .last
        .as_ref()
        .map(|l| list_time(l.timestamp, now))
        .unwrap_or_else(|| {
            if e.meta.last_ts > 0 {
                list_time(e.meta.last_ts, now)
            } else {
                String::new()
            }
        });

    let mut preview_row = row![].spacing(3).align_y(Alignment::Center);
    let typing = {
        let names: Vec<(String, crate::net::ChatState)> = app
            .typing
            .in_chat(&e.meta.id)
            .into_iter()
            .map(|(p, st)| (app.chats.plain_name(&p), st))
            .collect();
        crate::app::live::typing_label(&names, e.is_group())
    };
    if let Some(label) = typing {
        preview_row = preview_row.push(
            text(ellipsize(&label, 42))
                .size(m.text_small + 0.5)
                .color(p.accent)
                .wrapping(Wrapping::None)
                .shaping(text::Shaping::Advanced),
        );
    } else if let Some(d) = app.drafts.get(&e.meta.id).filter(|_| !selected) {
        // WhatsApp shows an unsent draft instead of the last message.
        preview_row = preview_row.push(
            text("Draft: ")
                .size(m.text_small + 0.5)
                .color(p.accent)
                .wrapping(Wrapping::None),
        );
        preview_row = preview_row.push(emoji_text(
            ellipsize(&d.replace('\n', " "), 36),
            m.text_small + 0.5,
            p.text_secondary,
            theme::INTER,
            false,
        ));
    } else if let Some(l) = &e.last {
        if l.from_me {
            preview_row = preview_row.push(ticks(t, l.status, 16.0));
        } else if e.is_group() {
            let who = app.chats.sender_name(&l.sender, l.sender_name.as_deref());
            preview_row = preview_row.push(
                text(format!("{}: ", ellipsize(&who, 14)))
                    .size(m.text_small + 0.5)
                    .color(p.text_secondary)
                    .wrapping(Wrapping::None),
            );
        }
        preview_row = preview_row.push(emoji_text(
            ellipsize(&app.preview(l), 42),
            m.text_small + 0.5,
            p.text_secondary,
            theme::INTER,
            false,
        ));
    }
    preview_row = preview_row.push(fill());
    let muted = app.is_muted(&e.meta.id);
    if muted {
        preview_row = preview_row.push(icon(Icon::BellOff, 15.0, p.text_secondary));
    }
    if e.meta.pinned {
        preview_row = preview_row.push(icon(Icon::Pin, 15.0, p.text_secondary));
    }
    if unread > 0 {
        preview_row = preview_row.push(gap(4.0)).push(badge(t, unread, muted));
    }

    let content = column![
        row![
            text(ellipsize(&name, 34))
                .size(16.5)
                .color(p.text_primary)
                .wrapping(Wrapping::None)
                .shaping(text::Shaping::Advanced),
            fill(),
            text(time).size(12).color(if unread > 0 && !muted {
                p.accent
            } else {
                p.text_secondary
            }),
        ]
        .align_y(Alignment::Center),
        preview_row,
    ]
    .spacing(3)
    .width(Length::Fill);

    let line = container(Space::new().height(1))
        .width(Length::Fill)
        .style(theme::solid(if selected {
            iced::Color::TRANSPARENT
        } else {
            p.border
        }));

    let inner = row![
        container(avatar_for(app, &e.meta.id, e.is_group(), m.avatar)).padding([0, 15]),
        column![
            Space::new().height(Length::Fill),
            container(content).padding(iced::Padding {
                top: 0.0,
                right: 15.0,
                bottom: 0.0,
                left: 0.0
            }),
            Space::new().height(Length::Fill),
            line,
        ]
        .height(m.row_height),
    ]
    .align_y(Alignment::Center);

    mouse_area(
        button(inner)
            .padding(0)
            .width(Length::Fill)
            .height(m.row_height)
            .style(theme::chat_row(t, selected))
            .on_press(Message::Ui(Ui::OpenChat(e.meta.id.clone()))),
    )
    .on_right_press(Message::Ui(Ui::ChatMenu(e.meta.id.clone())))
    .into()
}

/// The "Archived" row at the top of the chat list.
fn archived_row(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let unread = app.chats.archived_unread();
    let mut r = row![
        container(icon(Icon::Archive, 20.0, p.accent))
            .width(49)
            .center_x(49),
        text("Archived").size(16).color(p.text_primary),
        fill(),
    ]
    .spacing(15)
    .align_y(Alignment::Center);
    // WhatsApp Desktop: the number of archived chats with unread messages,
    // in green, no pill; nothing when none is unread.
    if unread > 0 {
        r = r.push(
            text(unread.to_string())
                .size(12)
                .font(theme::INTER_SEMIBOLD)
                .color(p.accent),
        );
    }
    button(r)
        .padding([14, 16])
        .width(Length::Fill)
        .style(theme::chat_row(t, false))
        .on_press(Message::Ui(Ui::OpenArchived))
        .into()
}

/// Filter chips: the built-in ones, the user's lists and "+".
fn chips(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let mut r = row![].spacing(8).align_y(Alignment::Center);
    for f in Filter::BUILTIN {
        let active = app.filter == f;
        r = r.push(
            button(text(f.label()).size(14).font(INTER_MEDIUM))
                .padding([6, 12])
                .style(theme::chip(t, active))
                .on_press(Message::Ui(Ui::Filter(f))),
        );
    }
    for l in app.chats.custom_lists() {
        let f = Filter::List(l.id.clone());
        let active = app.filter == f;
        r = r.push(
            mouse_area(
                button(
                    text(ellipsize(&l.name, 18))
                        .size(14)
                        .font(INTER_MEDIUM)
                        .shaping(text::Shaping::Advanced),
                )
                .padding([6, 12])
                .style(theme::chip(t, active))
                .on_press(Message::Ui(Ui::Filter(f))),
            )
            .on_right_press(Message::Ui(Ui::ChipMenu(l.id.clone()))),
        );
    }
    r = r.push(
        button(icon(Icon::Plus, 16.0, p.text_secondary))
            .padding([7, 10])
            .style(theme::chip(t, false))
            .on_press(Message::Ui(Ui::NewList)),
    );
    scrollable(r)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(0).scroller_width(0),
        ))
        .into()
}

/// Right-click menu of a chat row or a list chip, floating at the pointer.
pub fn context_menu(app: &App) -> Option<El<'_>> {
    let menu = app.sidebar_menu.as_ref()?;
    let t = &app.theme;
    let p = t.palette;
    let item = |label: String, i: Icon, on: Ui, danger: bool| -> El<'_> {
        button(
            row![
                icon(i, 17.0, if danger { p.danger } else { p.text_secondary }),
                text(label)
                    .size(14.5)
                    .color(if danger { p.danger } else { p.text_primary })
                    .shaping(text::Shaping::Advanced)
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([9, 18])
        .style(theme::chat_row(t, false))
        .on_press(Message::Ui(on))
        .into()
    };
    let mut col = column![];
    let mut rows = 0;
    match &menu.target {
        MenuTarget::Chat(chat) => {
            let e = app.chats.get(chat)?;
            let archived = e.meta.archived;
            col = col.push(item(
                if archived {
                    "Unarchive chat"
                } else {
                    "Archive chat"
                }
                .into(),
                Icon::Archive,
                Ui::ToggleArchive(chat.clone()),
                false,
            ));
            let muted = app.is_muted(chat);
            col = col.push(item(
                if muted {
                    "Unmute notifications"
                } else {
                    "Mute notifications"
                }
                .into(),
                Icon::BellOff,
                Ui::ChatAct(if muted {
                    ChatAct::Unmute(chat.clone())
                } else {
                    ChatAct::AskMute(chat.clone())
                }),
                false,
            ));
            col = col.push(item(
                if e.meta.pinned {
                    "Unpin chat"
                } else {
                    "Pin chat"
                }
                .into(),
                Icon::Pin,
                Ui::TogglePin(chat.clone()),
                false,
            ));
            let unread = e.meta.unread > 0;
            col = col.push(item(
                if unread {
                    "Mark as read"
                } else {
                    "Mark as unread"
                }
                .into(),
                if unread {
                    Icon::CheckCheck
                } else {
                    Icon::MessageCircle
                },
                Ui::ChatAct(if unread {
                    ChatAct::MarkRead(chat.clone())
                } else {
                    ChatAct::MarkUnread(chat.clone())
                }),
                false,
            ));
            col = col.push(item(
                if app.chats.is_favourite(chat) {
                    "Remove from Favourites"
                } else {
                    "Add to Favourites"
                }
                .into(),
                Icon::Star,
                Ui::ToggleFavourite(chat.clone()),
                false,
            ));
            rows += 5;
            for l in app.chats.custom_lists().take(8) {
                let member = app.chats.in_list(&l.id, chat);
                col = col.push(item(
                    if member {
                        format!("Remove from {}", ellipsize(&l.name, 20))
                    } else {
                        format!("Add to {}", ellipsize(&l.name, 20))
                    },
                    if member { Icon::X } else { Icon::Plus },
                    Ui::ToggleInList {
                        chat: chat.clone(),
                        list: l.id.clone(),
                    },
                    false,
                ));
                rows += 1;
            }
            col = col.push(item(
                "Clear chat".into(),
                Icon::CircleDashed,
                Ui::ChatAct(ChatAct::AskClear(chat.clone())),
                false,
            ));
            col = col.push(item(
                "Delete chat".into(),
                Icon::Trash2,
                Ui::ChatAct(ChatAct::AskDelete(chat.clone())),
                true,
            ));
            rows += 2;
        }
        MenuTarget::Chip(id) => {
            col = col.push(item(
                "Rename list".into(),
                Icon::Pencil,
                Ui::RenameList(id.clone()),
                false,
            ));
            col = col.push(item(
                "Delete list".into(),
                Icon::Trash2,
                Ui::AskDeleteList(id.clone()),
                true,
            ));
            rows += 2;
        }
    }
    let (w, h) = (MENU_W, 12.0 + 38.0 * rows as f32);
    let win = app.window_size();
    let x = menu.at.x.min(win.width - w - 8.0).max(8.0);
    let y = menu.at.y.min(win.height - h - 8.0).max(8.0);
    let card = container(col.width(w))
        .padding([6, 0])
        .style(move |th: &iced::Theme| {
            let mut s = theme::rounded(p.bg_elevated, 8.0)(th);
            s.shadow = iced::Shadow {
                color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.4),
                offset: iced::Vector::new(0.0, 2.0),
                blur_radius: 10.0,
            };
            s
        });
    let dismiss = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::Ui(Ui::CloseSidebarMenu))
        .on_right_press(Message::Ui(Ui::CloseSidebarMenu));
    Some(
        stack![
            dismiss,
            container(card).padding(iced::Padding {
                top: y,
                left: x,
                right: 0.0,
                bottom: 0.0,
            })
        ]
        .into(),
    )
}

/// Width of the context menu card.
const MENU_W: f32 = 240.0;

/// The create / rename / delete list dialog.
pub fn list_dialog(app: &App) -> Option<El<'_>> {
    let d = app.list_dialog.as_ref()?;
    let t = &app.theme;
    let p = t.palette;
    let btn = |label: &'static str, on: Option<Message>, danger: bool| -> El<'_> {
        let b = button(text(label).size(14.5).font(INTER_MEDIUM).color(if danger {
            p.danger
        } else {
            p.accent
        }))
        .padding([9, 16])
        .style(theme::link_button(t));
        match on {
            Some(m) => b.on_press(m).into(),
            None => b.into(),
        }
    };
    let body: El<'_> = match d {
        ListDialog::Edit { id, name } => {
            let valid = !name.trim().is_empty();
            column![
                text(if id.is_some() {
                    "Rename list"
                } else {
                    "New list"
                })
                .size(18)
                .color(p.text_primary),
                container(
                    text_input("List name", name)
                        .id(LIST_NAME_ID)
                        .on_input(|s| Message::Ui(Ui::ListName(s)))
                        .on_submit(Message::Ui(Ui::SaveList))
                        .size(t.metrics.text)
                        .padding([8, 12])
                        .style(theme::input(t)),
                )
                .style(theme::rounded(p.bg_input, 8.0)),
                text(format!("{} / {MAX_LIST_NAME}", name.chars().count()))
                    .size(12)
                    .color(p.text_secondary),
                row![
                    fill(),
                    btn("Cancel", Some(Message::Ui(Ui::CloseListDialog)), false),
                    btn("Save", valid.then(|| Message::Ui(Ui::SaveList)), false),
                ]
                .spacing(4),
            ]
            .spacing(12)
            .into()
        }
        ListDialog::Delete(id) => {
            let name = app
                .chats
                .list(id)
                .map(|l| l.name.as_str())
                .unwrap_or_default();
            column![
                text(format!("Delete \"{}\"?", ellipsize(name, 28)))
                    .size(18)
                    .color(p.text_primary)
                    .shaping(text::Shaping::Advanced),
                text("The chats stay where they are; only the list is removed.")
                    .size(14)
                    .color(p.text_secondary),
                row![
                    fill(),
                    btn("Cancel", Some(Message::Ui(Ui::CloseListDialog)), false),
                    btn(
                        "Delete",
                        Some(Message::Ui(Ui::ConfirmDeleteList(id.clone()))),
                        true
                    ),
                ]
                .spacing(4),
            ]
            .spacing(10)
            .into()
        }
    };
    Some(modal(app, body))
}

/// A dialog card centred over a dimmed window.
fn modal<'a>(app: &'a App, body: El<'a>) -> El<'a> {
    let p = app.theme.palette;
    let card = container(body)
        .padding(24)
        .width(380)
        .style(move |th: &iced::Theme| {
            let mut s = theme::rounded(p.bg_elevated, 12.0)(th);
            s.shadow = iced::Shadow {
                color: iced::Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                offset: iced::Vector::new(0.0, 4.0),
                blur_radius: 24.0,
            };
            s
        });
    opaque(
        container(card)
            .center(Length::Fill)
            .style(theme::solid(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.55))),
    )
}

/// The mute / clear / delete chat dialog.
pub fn chat_dialog(app: &App) -> Option<El<'_>> {
    let d = app.chat_dialog.as_ref()?;
    let t = &app.theme;
    let p = t.palette;
    let btn = |label: &'static str, on: ChatAct, danger: bool| -> El<'_> {
        button(text(label).size(14.5).font(INTER_MEDIUM).color(if danger {
            p.danger
        } else {
            p.accent
        }))
        .padding([9, 16])
        .style(theme::link_button(t))
        .on_press(Message::Ui(Ui::ChatAct(on)))
        .into()
    };
    let name = |chat: &str| ellipsize(&app.chats.display_name(chat), 28);
    let body: El<'_> = match d {
        ChatDialog::Mute { chat, choice } => {
            let mut col = column![
                text(format!("Mute notifications from {}", name(chat)))
                    .size(18)
                    .color(p.text_primary)
                    .shaping(text::Shaping::Advanced),
                text("Other participants will not see that you muted this chat.")
                    .size(14)
                    .color(p.text_secondary),
            ]
            .spacing(10);
            for c in MuteFor::ALL {
                col = col.push(
                    radio(c.label(), c, Some(*choice), |c| {
                        Message::Ui(Ui::ChatAct(ChatAct::PickMute(c)))
                    })
                    .size(18)
                    .spacing(14)
                    .text_size(15)
                    .style(theme::radio(t)),
                );
            }
            col.push(
                row![
                    fill(),
                    btn("Cancel", ChatAct::Cancel, false),
                    btn("Mute", ChatAct::ConfirmMute, false),
                ]
                .spacing(4),
            )
            .into()
        }
        ChatDialog::Clear { chat, keep_starred } => column![
            text(format!("Clear this chat with {}?", name(chat)))
                .size(18)
                .color(p.text_primary)
                .shaping(text::Shaping::Advanced),
            text("Messages will be removed from this device and your phone.")
                .size(14)
                .color(p.text_secondary),
            checkbox(*keep_starred)
                .label("Keep starred messages")
                .on_toggle(|v| Message::Ui(Ui::ChatAct(ChatAct::KeepStarred(v))))
                .size(18)
                .spacing(14)
                .text_size(15)
                .style(theme::checkbox(t)),
            row![
                fill(),
                btn("Cancel", ChatAct::Cancel, false),
                btn("Clear chat", ChatAct::ConfirmClear, true),
            ]
            .spacing(4),
        ]
        .spacing(12)
        .into(),
        ChatDialog::Delete { chat } => column![
            text(format!("Delete this chat with {}?", name(chat)))
                .size(18)
                .color(p.text_primary)
                .shaping(text::Shaping::Advanced),
            text("Messages will be removed from this device and your phone.")
                .size(14)
                .color(p.text_secondary),
            row![
                fill(),
                btn("Cancel", ChatAct::Cancel, false),
                btn("Delete chat", ChatAct::ConfirmDelete, true),
            ]
            .spacing(4),
        ]
        .spacing(10)
        .into(),
    };
    Some(modal(app, body))
}
