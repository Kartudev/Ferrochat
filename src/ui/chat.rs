//! Conversation panel: header, bubbles, composer; empty state; image viewer.

use iced::widget::text::Wrapping;
use iced::widget::{
    Space, button, center, checkbox, column, container, image, mouse_area, opaque, row, scrollable,
    text, text_input,
};
use iced::{Alignment, Color, ContentFit, Length};

use super::emoji_font::{emoji_font, emoji_text};
use super::icons::Icon;
use super::theme::{self, FerroTheme, INTER_MEDIUM};
use super::{
    El, avatar_for, clock, day_separator, fill, gap, icon, last_seen_label, local_day, tail, ticks,
};
use crate::app::composer;
use crate::app::forward::Fwd;
use crate::app::live::typing_label;
use crate::app::state::{Conversation, reaction_summary};
use crate::app::{
    ATTACH_CAPTION_ID, App, CHAT_SEARCH_ID, COMPOSER_ID, CONVERSATION_SCROLL, Dialog, Message,
    Popup, Ui, row_id,
};
use crate::media::image::human_size;
use crate::net::{
    ChatState, Content, Message as ChatMessage, Quote, ReceiptStatus, format_duration, is_group_jid,
};

/// Quick reactions offered on hover (the official client's defaults).
pub const QUICK_REACTIONS: &[&str] = &["👍", "❤️", "😂", "😮", "😢", "🙏"];

/// Placeholder when no chat is open.
pub fn empty(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    container(
        column![
            container(icon(Icon::Laptop, 64.0, p.text_secondary))
                .padding(28)
                .style(theme::rounded(p.bg_elevated, 60.0)),
            text("Ferrochat for Linux").size(30).color(p.text_primary),
            text(
                "Send and receive messages from your computer. Your phone stays the \
                 primary device; Ferrochat is linked to it."
            )
            .size(14)
            .color(p.text_secondary)
            .align_x(Alignment::Center)
            .width(460),
            Space::new().height(Length::Fill).height(40),
            row![
                icon(Icon::Lock, 13.0, p.text_secondary),
                text("Your personal messages are end-to-end encrypted")
                    .size(13)
                    .color(p.text_secondary),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        ]
        .spacing(18)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .style(theme::bordered(
        p.bg_elevated,
        p.notice_text.scale_alpha(0.0),
    ))
    .into()
}

/// The open conversation.
pub fn view<'a>(app: &'a App, conv: &'a Conversation) -> El<'a> {
    let el = view_inner(app, conv);
    if !app.drop.hovering {
        return el;
    }
    iced::widget::stack![el, drop_overlay(app)].into()
}

/// Shown over the conversation while files are dragged over the window.
fn drop_overlay(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let card = container(
        column![
            icon(Icon::Paperclip, 44.0, p.accent),
            text("Drop files here to send them")
                .size(m.text + 3.0)
                .color(p.text_primary),
            text("Photos, videos, GIFs and documents")
                .size(m.text - 1.0)
                .color(p.text_secondary),
        ]
        .spacing(12)
        .align_x(Alignment::Center),
    )
    .padding([36, 48])
    .style(move |th: &iced::Theme| {
        let mut s = theme::rounded(p.bg_elevated, 16.0)(th);
        s.border = iced::Border {
            color: p.accent,
            width: 2.0,
            radius: 16.0.into(),
        };
        s
    });
    container(card)
        .center(Length::Fill)
        .style(theme::solid(Color::from_rgba(0.0, 0.0, 0.0, 0.55)))
        .into()
}

fn view_inner<'a>(app: &'a App, conv: &'a Conversation) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let group = is_group_jid(&conv.chat);
    let name = app.chats.display_name(&conv.chat);

    let typing_names: Vec<(String, ChatState)> = app
        .typing
        .in_chat(&conv.chat)
        .into_iter()
        .map(|(p, st)| (app.chats.plain_name(&p), st))
        .collect();
    let typing = typing_label(&typing_names, group);
    let now = chrono::Utc::now().timestamp();
    let subtitle = if let Some(t) = &typing {
        t.clone()
    } else if group {
        let mut senders: Vec<String> = Vec::new();
        for msg in conv.messages.iter().rev().filter(|m| !m.from_me) {
            let n = app
                .chats
                .resolve(&msg.sender, msg.sender_name.as_deref())
                .plain();
            if !senders.contains(&n) {
                senders.push(n);
            }
            if senders.len() == 5 {
                break;
            }
        }
        senders.push("You".into());
        senders.join(", ")
    } else {
        match app.presence.get(&conv.chat) {
            Some(p) if p.online => "online".into(),
            Some(p) if p.last_seen.is_some() => {
                last_seen_label(p.last_seen.unwrap_or_default(), now)
            }
            _ => "click here for contact info".into(),
        }
    };
    let subtitle_color = if typing.is_some() || subtitle == "online" {
        p.accent
    } else {
        p.text_secondary
    };

    // The avatar, name and subtitle open the info drawer, like WhatsApp
    // Desktop's header (the subtitle invites the click).
    let who = button(
        row![
            avatar_for(app, &conv.chat, group, m.avatar_small),
            column![
                emoji_text(name, 16.0, p.text_primary, theme::INTER, false),
                text(super::ellipsize(&subtitle, 80))
                    .size(13)
                    .color(subtitle_color)
                    .wrapping(Wrapping::None),
            ]
            .spacing(1),
            fill(),
        ]
        .spacing(15)
        .align_y(Alignment::Center),
    )
    .padding(0)
    .width(Length::Fill)
    .style(theme::flat_button(p.text_primary))
    .on_press(Message::Ui(Ui::ToggleInfo));
    let header = container(
        row![
            who,
            super::call::header_buttons(app, &conv.chat),
            button(icon(Icon::Search, m.icon, p.text_secondary))
                .padding(8)
                .style(theme::icon_button(t, app.chat_search.is_some()))
                .on_press(Message::Ui(Ui::ToggleChatSearch)),
            button(icon(Icon::EllipsisVertical, m.icon, p.text_secondary))
                .padding(8)
                .style(theme::icon_button(t, false)),
        ]
        .spacing(15)
        .align_y(Alignment::Center),
    )
    .height(m.header_height)
    .padding([0, 16])
    .align_y(Alignment::Center)
    .style(theme::solid(p.bg_elevated));

    // Messages
    let mut items: Vec<El<'a>> = Vec::with_capacity(conv.messages.len() + 8);
    items.push(
        container(
            container(
                row![
                    icon(Icon::Lock, 12.0, p.notice_text),
                    text(
                        "Messages are end-to-end encrypted. No one outside of this chat, not \
                         even WhatsApp, can read or listen to them."
                    )
                    .size(12.5)
                    .color(p.notice_text)
                    .align_x(Alignment::Center),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding([6, 12])
            .max_width(520)
            .style(theme::rounded(p.system_chip, 7.5)),
        )
        .center_x(Length::Fill)
        .padding([10, 0])
        .into(),
    );
    if conv.has_more {
        items.push(
            container(
                text("Scroll up to load older messages")
                    .size(12)
                    .color(p.text_secondary),
            )
            .center_x(Length::Fill)
            .into(),
        );
    }
    let mut prev: Option<&ChatMessage> = None;
    for msg in &conv.messages {
        let new_day = prev.is_none_or(|pm| local_day(pm.timestamp) != local_day(msg.timestamp));
        if new_day {
            items.push(
                container(
                    container(
                        text(day_separator(msg.timestamp, now))
                            .size(12.5)
                            .color(p.text_secondary),
                    )
                    .padding([5, 12])
                    .style(theme::rounded(p.system_chip, 7.5)),
                )
                .center_x(Length::Fill)
                .padding([10, 0])
                .into(),
            );
        }
        if let Some(d) = conv
            .unread
            .as_ref()
            .filter(|d| d.first.as_deref() == Some(msg.id.as_str()))
        {
            items.push(
                container(
                    text(format!(
                        "{} unread message{}",
                        d.count,
                        if d.count == 1 { "" } else { "s" }
                    ))
                    .size(12.5)
                    .color(p.text_primary),
                )
                .padding([5, 12])
                .center_x(Length::Fill)
                .style(theme::solid(p.bg_elevated.scale_alpha(0.7)))
                .into(),
            );
        }
        let first_of_group =
            new_day || prev.is_none_or(|pm| pm.from_me != msg.from_me || pm.sender != msg.sender);
        items.push(bubble(app, msg, first_of_group, group));
        prev = Some(msg);
    }

    let messages = scrollable(column(items).spacing(2).padding(iced::Padding {
        top: 4.0,
        right: 56.0,
        bottom: 12.0,
        left: 56.0,
    }))
    .id(CONVERSATION_SCROLL)
    .direction(super::thin_scrollbar())
    .anchor_bottom()
    .on_scroll(|v| {
        Message::Ui(Ui::Scrolled {
            relative_y: v.relative_offset().y,
            offset_y: v.absolute_offset().y,
        })
    })
    .style(theme::scrollbar(t))
    .height(Length::Fill);

    let background = container(messages)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::solid(p.bg_panel));

    if let Some(a) = app.attach.as_ref().filter(|a| a.chat == conv.chat) {
        return with_info(app, conv, column![header, attach_preview(app, a)].into());
    }
    let mut body = column![header];
    if let Some(cs) = &app.chat_search {
        body = body.push(search_bar(app, cs));
    }
    let mut layers: Vec<El<'a>> = vec![background.into()];
    if conv.has_newer {
        layers.push(
            container(
                button(icon(Icon::ChevronDown, 22.0, p.text_secondary))
                    .padding(10)
                    .style(theme::icon_button(t, false))
                    .on_press(Message::Ui(Ui::JumpLatest)),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::End)
            .align_y(Alignment::End)
            .padding(16)
            .into(),
        );
    }
    if app.emoji_open {
        layers.push(super::emoji_picker::view(app));
    }
    if let (Some((id, which)), Some(pos)) = (&app.popup, app.popup_pos)
        && let Some(msg) = conv.get(id)
    {
        layers.push(popup_layer(app, msg, *which, pos));
    }
    body = body.push(iced::widget::stack(layers));
    if let Some(sel) = app.selection.as_ref().filter(|s| s.chat == conv.chat) {
        return with_info(app, conv, body.push(selection_bar(app, sel)).into());
    }
    if let Some(bar) = reply_bar(app, conv) {
        body = body.push(bar);
    }
    with_info(app, conv, body.push(composer(app)).into())
}

/// Replaces the composer in selection mode: close, count, forward.
fn selection_bar<'a>(app: &'a App, sel: &'a crate::app::forward::Selection) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let n = sel.ids.len();
    let mut fwd = button(icon(
        Icon::Forward,
        m.icon,
        if n > 0 {
            p.text_primary
        } else {
            p.text_secondary
        },
    ))
    .padding(8)
    .style(theme::icon_button(t, false));
    if n > 0 {
        fwd = fwd.on_press(Message::Ui(Ui::Forward(Fwd::Selected)));
    }
    container(
        row![
            button(icon(Icon::X, m.icon, p.text_secondary))
                .padding(8)
                .style(theme::icon_button(t, false))
                .on_press(Message::Ui(Ui::Forward(Fwd::CancelSelect))),
            text(format!("{n} selected")).size(15).color(p.text_primary),
            fill(),
            fwd,
        ]
        .spacing(14)
        .align_y(Alignment::Center),
    )
    .height(62)
    .align_y(Alignment::Center)
    .padding([8, 16])
    .style(theme::solid(p.bg_elevated))
    .into()
}

/// The conversation with the info drawer on its right, when open.
fn with_info<'a>(app: &'a App, conv: &'a Conversation, el: El<'a>) -> El<'a> {
    if let Some(mi) = app.msg_info.as_ref().filter(|i| i.chat == conv.chat)
        && let Some(msg) = conv.get(&mi.id)
    {
        return row![
            container(el).width(Length::Fill).height(Length::Fill),
            super::msg_info::view(app, msg)
        ]
        .into();
    }
    match app.info.as_ref().filter(|i| i.chat == conv.chat) {
        Some(info) => row![
            container(el).width(Length::Fill).height(Length::Fill),
            super::info::view(app, info)
        ]
        .into(),
        None => el,
    }
}

/// Floating reaction picker / message menu next to its message row. A
/// transparent layer under the card closes it on any outside click.
fn popup_layer<'a>(
    app: &'a App,
    msg: &'a ChatMessage,
    which: Popup,
    pos: crate::app::PopupPos,
) -> El<'a> {
    let now = chrono::Utc::now().timestamp();
    let (card, card_h) = match which {
        Popup::Reactions => (reaction_picker(app, msg), 52.0),
        Popup::Menu => {
            // Reply, React, Copy, Star, Select messages, Delete.
            let items = 6
                + usize::from(msg.can_forward())
                + usize::from(msg.can_edit(now))
                + usize::from(msg.from_me)
                + usize::from(matches!(msg.content, Content::Video { .. }));
            (message_menu(app, msg), 12.0 + 38.0 * items as f32)
        }
    };
    let top = crate::app::popup_top(pos, card_h);
    let side = if msg.from_me {
        Alignment::End
    } else {
        Alignment::Start
    };
    let dismiss = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::Ui(Ui::ClosePopup));
    iced::widget::stack![
        dismiss,
        container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(side)
            .padding(iced::Padding {
                top,
                right: 76.0,
                bottom: 0.0,
                left: 76.0,
            })
    ]
    .into()
}

/// The in-chat search bar under the header.
fn search_bar<'a>(app: &'a App, cs: &'a crate::app::ChatSearch) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let status = if cs.query.trim().chars().count() < 2 {
        String::new()
    } else if cs.hits.is_empty() {
        "No results".to_string()
    } else {
        format!("{} of {}", cs.index + 1, cs.hits.len())
    };
    container(
        row![
            container(
                row![
                    icon(Icon::Search, 16.0, p.text_secondary),
                    text_input("Search in this chat", &cs.query)
                        .id(CHAT_SEARCH_ID)
                        .on_input(|s| Message::Ui(Ui::ChatSearch(s)))
                        .on_submit(Message::Ui(Ui::ChatSearchStep { delta: 1 }))
                        .size(t.metrics.text)
                        .padding([6, 0])
                        .style(theme::input(t)),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            )
            .padding([2, 14])
            .width(Length::Fill)
            .style(theme::rounded(p.bg_input, 18.0)),
            text(status).size(13).color(p.text_secondary),
            button(icon(Icon::ChevronUp, 20.0, p.text_secondary))
                .padding(6)
                .style(theme::icon_button(t, false))
                .on_press(Message::Ui(Ui::ChatSearchStep { delta: 1 })),
            button(icon(Icon::ChevronDown, 20.0, p.text_secondary))
                .padding(6)
                .style(theme::icon_button(t, false))
                .on_press(Message::Ui(Ui::ChatSearchStep { delta: -1 })),
            button(icon(Icon::X, 20.0, p.text_secondary))
                .padding(6)
                .style(theme::icon_button(t, false))
                .on_press(Message::Ui(Ui::ToggleChatSearch)),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding([8, 16])
    .style(theme::solid(p.bg_sidebar))
    .into()
}

/// Reply / edit bar above the composer.
fn reply_bar<'a>(app: &'a App, conv: &'a Conversation) -> Option<El<'a>> {
    let t = &app.theme;
    let p = t.palette;
    let (label, label_color, preview, close, lead): (String, Color, String, Ui, Option<El<'a>>) =
        if let Some(id) = &app.editing {
            let text = conv
                .get(id)
                .and_then(|m| m.content.text())
                .map(str::to_string)
                .unwrap_or_default();
            (
                "Edit message".into(),
                p.accent,
                text,
                Ui::CancelEdit,
                Some(icon(Icon::Pencil, 18.0, p.text_secondary)),
            )
        } else if let Some(q) = &app.reply_to {
            (
                quote_author(app, q),
                quote_color(app, q),
                q.preview.clone(),
                Ui::CancelReply,
                None,
            )
        } else {
            return None;
        };
    let mut inner = row![
        container(Space::new().width(4).height(Length::Fill)).style(theme::solid(label_color)),
    ]
    .spacing(10)
    .height(Length::Shrink);
    if let Some(l) = lead {
        inner = inner.push(l);
    }
    inner = inner.push(
        column![
            text(label)
                .size(13.5)
                .font(INTER_MEDIUM)
                .color(label_color)
                .shaping(text::Shaping::Advanced),
            text(super::ellipsize(&preview, 110))
                .size(13.5)
                .color(p.text_secondary)
                .wrapping(Wrapping::None)
                .shaping(text::Shaping::Advanced),
        ]
        .spacing(2)
        .width(Length::Fill),
    );
    Some(
        container(
            row![
                container(inner.align_y(Alignment::Center))
                    .padding([8, 12])
                    .width(Length::Fill)
                    .height(56)
                    .style(theme::rounded(p.inset_in, 8.0)),
                button(icon(Icon::X, 20.0, p.text_secondary))
                    .padding(8)
                    .style(theme::icon_button(t, false))
                    .on_press(Message::Ui(close)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(iced::Padding {
            top: 8.0,
            right: 16.0,
            bottom: 0.0,
            left: 16.0,
        })
        .style(theme::solid(p.bg_elevated))
        .into(),
    )
}

/// Accent bar colour of a quote: green for ourselves, the sender colour otherwise.
fn quote_color(app: &App, q: &Quote) -> Color {
    if app.own_jid.as_deref() == Some(q.sender.as_str()) {
        app.theme.palette.accent
    } else {
        theme::sender_color(&q.sender)
    }
}

/// "You" or the display name of a quoted message's author.
fn quote_author(app: &App, q: &Quote) -> String {
    if app.own_jid.as_deref() == Some(q.sender.as_str()) {
        "You".into()
    } else {
        app.chats.display_name(&q.sender)
    }
}

/// Attachment preview (picked or pasted media): replaces the messages and
/// the composer until the media is sent or discarded, like WhatsApp Desktop.
fn attach_preview<'a>(app: &'a App, a: &'a crate::app::paste::AttachPreview) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let Some(first) = a.items.first() else {
        return Space::new().into();
    };
    let title = match a.items.len() {
        1 => first.file_name.clone(),
        n => format!("{} and {} more", first.file_name, n - 1),
    };
    let top = row![
        button(icon(Icon::X, 22.0, p.text_secondary))
            .padding(8)
            .style(theme::icon_button(t, false))
            .on_press(Message::Ui(Ui::AttachCancel)),
        text(title)
            .size(m.text)
            .color(p.text_primary)
            .wrapping(Wrapping::None),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .padding([8, 16]);
    let content: El<'a> = match &a.handle {
        Some(h) => match &first.kind {
            crate::net::OutgoingKind::Video { seconds, gif, .. } => {
                // The first frame with what WhatsApp shows on a video: a
                // play mark and the duration, or "GIF".
                let shade = Color::from_rgba(0.0, 0.0, 0.0, 0.55);
                let label: El<'a> = if *gif {
                    text("GIF").size(13).color(Color::WHITE).into()
                } else {
                    row![
                        icon(Icon::Video, 13.0, Color::WHITE),
                        text(format_duration(*seconds)).size(13).color(Color::WHITE),
                    ]
                    .spacing(5)
                    .align_y(Alignment::Center)
                    .into()
                };
                column![
                    image(h.clone()).content_fit(ContentFit::ScaleDown),
                    container(label)
                        .padding([3, 9])
                        .style(theme::rounded(shade, 10.0)),
                ]
                .spacing(10)
                .align_x(Alignment::Center)
                .into()
            }
            _ => image(h.clone()).content_fit(ContentFit::ScaleDown).into(),
        },
        None => column![
            icon(Icon::FileText, 72.0, p.text_secondary),
            text(first.file_name.clone())
                .size(m.text + 1.0)
                .color(p.text_primary),
            text(human_size(first.bytes.len() as u64))
                .size(m.text - 1.0)
                .color(p.text_secondary),
        ]
        .spacing(10)
        .align_x(Alignment::Center)
        .into(),
    };
    let caption = container(
        text_input("Add a caption", &a.caption)
            .id(ATTACH_CAPTION_ID)
            .on_input(|s| Message::Ui(Ui::AttachCaption(s)))
            .on_submit(Message::Ui(Ui::AttachSend))
            .size(m.text + 0.5)
            .padding([9, 12])
            .style(theme::input(t)),
    )
    .padding([2, 8])
    .width(Length::Fill)
    .style(theme::rounded(p.bg_selected, 22.0));
    let send = button(icon(Icon::SendHorizontal, 20.0, p.accent_text))
        .padding(11)
        .style(theme::primary_button(t))
        .on_press(Message::Ui(Ui::AttachSend));
    container(column![
        top,
        center(content).padding(32),
        container(row![caption, send].spacing(10).align_y(Alignment::Center))
            .padding([12, 16])
            .style(theme::solid(p.bg_elevated)),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::solid(p.bg_panel))
    .into()
}

fn composer(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    if let Some(since) = app.recording_since {
        let secs = since.elapsed().as_secs() as u32;
        return container(
            row![
                fill(),
                button(icon(Icon::Trash2, m.icon, p.text_secondary))
                    .padding(8)
                    .style(theme::icon_button(t, false))
                    .on_press(Message::Ui(Ui::StopRecording { send: false })),
                container(Space::new().width(10).height(10)).style(theme::rounded(p.danger, 5.0)),
                text(format_duration(secs)).size(16).color(p.text_primary),
                button(icon(Icon::SendHorizontal, 20.0, p.accent_text))
                    .padding(11)
                    .style(theme::primary_button(t))
                    .on_press(Message::Ui(Ui::StopRecording { send: true })),
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        )
        .padding([8, 16])
        .style(theme::solid(p.bg_elevated))
        .into();
    }
    let has_text = !app.composer.is_blank();
    // Enter sends, Shift+Enter breaks the line; the box grows with its text
    // up to `composer::MAX_LINES` and then scrolls.
    let (min_h, max_h) = composer::text_height_limits(app.window_size().height);
    let input = iced::widget::text_editor(app.composer.content())
        .id(COMPOSER_ID)
        .placeholder(if app.editing.is_some() {
            "Edit message"
        } else {
            "Type a message"
        })
        .on_action(|a| Message::Ui(Ui::ComposerAction(a)))
        .key_binding(|kp| composer::key_binding(kp, Message::Ui(Ui::Send)))
        .size(m.text + 0.5)
        .line_height(text::LineHeight::Absolute(composer::LINE_HEIGHT.into()))
        .padding(iced::Padding::from([composer::PADDING_Y, 0.0]))
        .min_height(min_h)
        .max_height(max_h)
        .style(theme::editor(t));
    // The tools stay level with the last line of a tall box.
    let tool_pad = iced::Padding::ZERO.bottom(2.0);
    let field = container(
        row![
            button(icon(Icon::Plus, 24.0, p.text_secondary))
                .padding(6)
                .style(theme::icon_button(t, false))
                .on_press(Message::Ui(Ui::Attach)),
            button(icon(
                Icon::Smile,
                22.0,
                if app.emoji_open {
                    p.accent
                } else {
                    p.text_secondary
                }
            ))
            .padding(6)
            .style(theme::icon_button(t, false))
            .on_press(Message::Ui(Ui::ToggleEmoji)),
            input,
        ]
        .spacing(4)
        .padding(tool_pad)
        .align_y(Alignment::End),
    )
    .padding([2, 8])
    .width(Length::Fill)
    .style(theme::rounded(p.bg_selected, 22.0));
    let action: El<'_> = if has_text {
        button(icon(Icon::SendHorizontal, 20.0, p.accent_text))
            .padding(11)
            .style(theme::primary_button(t))
            .on_press(Message::Ui(Ui::Send))
            .into()
    } else {
        button(icon(Icon::Mic, 22.0, p.text_secondary))
            .padding(10)
            .style(theme::icon_button(t, false))
            .on_press(Message::Ui(Ui::StartRecording))
            .into()
    };
    container(row![field, action].spacing(10).align_y(Alignment::End))
        .padding([8, 16])
        .style(theme::solid(p.bg_elevated))
        .into()
}

fn meta_row<'a>(t: &FerroTheme, msg: &ChatMessage) -> El<'a> {
    let p = t.palette;
    let color = if msg.from_me {
        p.bubble_out_meta
    } else {
        p.text_secondary
    };
    let mut r = row![].spacing(3).align_y(Alignment::Center);
    if msg.edited {
        r = r.push(text("Edited").size(t.metrics.text_tiny).color(color));
    }
    r = r.push(
        text(clock(msg.timestamp))
            .size(t.metrics.text_tiny)
            .color(color),
    );
    if msg.from_me {
        r = r.push(ticks(t, msg.status, 15.0));
    }
    r.into()
}

/// "Forwarded" / "Forwarded many times" above the content, like the
/// official client (one arrow, two for frequently forwarded messages).
fn forwarded_label<'a>(t: &FerroTheme, msg: &ChatMessage) -> El<'a> {
    let p = t.palette;
    let color = if msg.from_me {
        p.bubble_out_meta
    } else {
        p.text_secondary
    };
    let many = msg.frequently_forwarded();
    let mut arrows = row![icon(Icon::Forward, 14.0, color)];
    if many {
        arrows = arrows.push(icon(Icon::Forward, 14.0, color));
    }
    row![
        arrows,
        text(if many {
            "Forwarded many times"
        } else {
            "Forwarded"
        })
        .size(12.5)
        .font(theme::INTER)
        .color(color)
    ]
    .spacing(4)
    .align_y(Alignment::Center)
    .into()
}

fn bubble<'a>(app: &'a App, msg: &'a ChatMessage, first: bool, group: bool) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let out = msg.from_me;
    let fg = p.text_primary;

    let mut col = column![].spacing(3);
    if group && !out && first {
        let who = app
            .chats
            .sender_name(&msg.sender, msg.sender_name.as_deref());
        col = col.push(emoji_text(
            who,
            12.5,
            theme::sender_color(&msg.sender),
            INTER_MEDIUM,
            false,
        ));
    }

    if msg.forwarded.is_some() && !matches!(msg.content, Content::Deleted) {
        col = col.push(forwarded_label(t, msg));
    }

    if let Some(q) = &msg.reply
        && !matches!(msg.content, Content::Deleted)
    {
        // Fill-width quote: the bubble column is compressed by the bubble
        // (see `finish`), so it gets the widest non-Fill child's width.
        col = col.push(quote_block(app, msg, q)).push(quote_sizer(app, q));
    }

    let body: El<'a> = match &msg.content {
        Content::Text(s) => {
            // Short messages keep the time on the same line, like the original.
            let short = s.chars().count() < 38 && !s.contains('\n') && msg.reply.is_none();
            let txt = message_text(app, msg, s, fg);
            if short {
                return finish(
                    app,
                    msg,
                    first,
                    col.push(
                        row![txt, gap(10.0), container(meta_row(t, msg)).padding(iced::Padding {
                            top: 6.0,
                            ..Default::default()
                        })]
                        .align_y(Alignment::End),
                    )
                    .into(),
                );
            }
            column![txt, container(meta_row(t, msg)).align_right(Length::Fill)]
                .spacing(1)
                .width(Length::Shrink)
                .into()
        }
        Content::Link { text: body_text, preview } => {
            let inset = if out { p.inset_out } else { p.inset_in };
            let sub = if out { p.bubble_out_meta } else { p.text_secondary };
            let mut card = column![].spacing(2);
            if let Some(h) = app.thumbs.get(&msg.id) {
                card = card.push(cover(
                    image(h.clone())
                        .width(Length::Fill)
                        .height(150)
                        .content_fit(ContentFit::Contain)
                        .border_radius(6.0),
                ));
            }
            let mut info = column![
                text(super::ellipsize(&preview.title, 90))
                    .size(m.text)
                    .font(INTER_MEDIUM)
                    .color(fg)
                    .shaping(text::Shaping::Advanced)
            ]
            .spacing(2)
            .padding([6, 8]);
            if !preview.description.is_empty() {
                info = info.push(
                    text(super::ellipsize(&preview.description, 110))
                        .size(m.text_small)
                        .color(sub)
                        .shaping(text::Shaping::Advanced),
                );
            }
            info = info.push(
                text(super::ellipsize(&link_host(&preview.url), 40))
                    .size(m.text_small)
                    .color(sub)
                    .wrapping(Wrapping::WordOrGlyph),
            );
            card = card.push(info);
            // Full bubble width like the official app, at least 300 px.
            let card = button(card)
                .padding(0)
                .width(Length::Fill)
                .style(move |th, st| {
                    let mut s = theme::flat_button(fg)(th, st);
                    s.background = Some(iced::Background::Color(inset));
                    s.border.radius = 6.0.into();
                    s
                })
                .on_press(Message::Ui(Ui::OpenLink(preview.url.clone())));
            col = col.push(card).push(Space::new().width(300));
            column![
                message_text(app, msg, body_text, fg),
                container(meta_row(t, msg)).align_right(Length::Fill)
            ]
            .spacing(1)
            .width(Length::Shrink)
            .into()
        }
        Content::Image {
            caption,
            width,
            height,
            media,
            ..
        } => {
            let (w, h) = fit(*width, *height, 330.0, 330.0);
            let handle = media
                .as_ref()
                .and_then(|r| app.images.peek(&r.cache_key()))
                .or_else(|| app.thumbs.get(&msg.id))
                .cloned();
            let pic: El<'a> = match handle {
                Some(handle) => cover(
                    image(handle)
                        .width(w)
                        .height(h)
                        .content_fit(ContentFit::Contain)
                        .border_radius(6.0),
                ),
                None => container(icon(Icon::Image, 40.0, p.text_secondary))
                    .width(w)
                    .height(h)
                    .center_x(w)
                    .center_y(h)
                    .style(theme::rounded(p.inset_in, 6.0))
                    .into(),
            };
            let pic = button(pic)
                .padding(0)
                .style(theme::flat_button(fg))
                .on_press(Message::Ui(Ui::OpenImage(msg.id.clone())));
            let mut c = column![pic].spacing(4).width(w);
            if let Some(cap) = caption {
                c = c.push(message_text(app, msg, cap, fg));
            }
            c.push(container(meta_row(t, msg)).align_right(Length::Fill)).into()
        }
        Content::Document {
            file_name,
            mimetype,
            size,
            pages,
            caption,
            ..
        } => {
            let ext = file_name
                .rsplit_once('.')
                .map(|(_, e)| e.to_uppercase())
                .unwrap_or_else(|| mimetype.rsplit('/').next().unwrap_or("FILE").to_uppercase());
            let mut info = Vec::new();
            if let Some(pg) = pages {
                info.push(format!("{pg} page{}", if *pg == 1 { "" } else { "s" }));
            }
            info.push(ext.clone());
            info.push(human_size(*size));
            let inset = if out { p.inset_out } else { p.inset_in };
            let card = button(
                row![
                    container(
                        column![
                            icon(Icon::FileText, 26.0, Color::WHITE),
                        ]
                        .align_x(Alignment::Center)
                    )
                    .padding([6, 6])
                    .style(theme::rounded(doc_color(&ext), 4.0)),
                    column![
                        text(super::ellipsize(file_name, 60))
                            .size(m.text)
                            .color(fg)
                            .wrapping(Wrapping::WordOrGlyph),
                        text(info.join(" • ")).size(12).color(if out {
                            p.bubble_out_meta
                        } else {
                            p.text_secondary
                        }),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    container(icon(Icon::Download, 18.0, p.text_secondary))
                        .padding(7)
                        .style(move |th: &iced::Theme| {
                            let mut s = theme::rounded(Color::TRANSPARENT, 20.0)(th);
                            s.border.width = 1.5;
                            s.border.color = p.text_secondary.scale_alpha(0.6);
                            s
                        }),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding([10, 10])
            .width(300)
            .style(move |th, st| {
                let mut s = theme::flat_button(fg)(th, st);
                s.background = Some(iced::Background::Color(inset));
                s
            })
            .on_press(Message::Ui(Ui::OpenDocument(msg.id.clone())));
            let mut c = column![card].spacing(4);
            if let Some(cap) = caption {
                c = c.push(
                    text(cap.as_str())
                        .size(m.text)
                        .color(fg)
                        .wrapping(Wrapping::WordOrGlyph),
                );
            }
            c.push(container(meta_row(t, msg)).align_right(Length::Fill))
                .width(Length::Shrink)
                .into()
        }
        Content::Video {
            caption,
            seconds,
            width,
            height,
            gif,
            ..
        } => {
            let (w, h) = fit(*width, *height, 330.0, 330.0);
            let pic: El<'a> = match app.thumbs.get(&msg.id) {
                Some(handle) => cover(
                    image(handle.clone())
                        .width(w)
                        .height(h)
                        .content_fit(ContentFit::Contain)
                        .border_radius(6.0),
                ),
                None => container(icon(Icon::Video, 40.0, p.text_secondary))
                    .width(w)
                    .height(h)
                    .center_x(w)
                    .center_y(h)
                    .style(theme::rounded(p.inset_in, 6.0))
                    .into(),
            };
            let shade = Color::from_rgba(0.0, 0.0, 0.0, 0.55);
            let play = container(icon(Icon::Play, 26.0, Color::WHITE))
                .padding(14)
                .style(theme::rounded(shade, 30.0));
            // GIFs say so instead of a duration, like WhatsApp.
            let badge_label: El<'a> = if *gif {
                text("GIF").size(12).color(Color::WHITE).into()
            } else {
                row![
                    icon(Icon::Video, 12.0, Color::WHITE),
                    text(format_duration(*seconds)).size(12).color(Color::WHITE),
                ]
                .spacing(4)
                .align_y(Alignment::Center)
                .into()
            };
            let badge = container(badge_label)
            .padding([2, 7])
            .style(theme::rounded(shade, 9.0));
            let layered = iced::widget::stack![
                pic,
                container(play).center(Length::Fill),
                container(badge)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_y(Alignment::End)
                    .padding(6),
            ]
            .width(w)
            .height(h);
            let card = button(layered)
                .padding(0)
                .style(theme::flat_button(fg))
                .on_press(Message::Ui(Ui::OpenVideo(msg.id.clone())));
            let mut c = column![card].spacing(4).width(w);
            if let Some(cap) = caption {
                c = c.push(message_text(app, msg, cap, fg));
            }
            c.push(container(meta_row(t, msg)).align_right(Length::Fill)).into()
        }
        Content::Sticker {
            width,
            height,
            media,
            ..
        } => {
            let (w, h) = fit(*width, *height, 170.0, 170.0);
            let (w, h) = (w.min(170.0), h.min(170.0));
            let sticker: El<'a> = match media
                .as_ref()
                .and_then(|r| app.images.peek(&r.cache_key()))
            {
                Some(handle) => image(handle.clone())
                    .width(w)
                    .height(h)
                    .content_fit(ContentFit::Contain)
                    .into(),
                None => container(icon(Icon::Smile, 44.0, p.text_secondary))
                    .width(w)
                    .height(h)
                    .center_x(w)
                    .center_y(h)
                    .style(theme::rounded(p.inset_in, 12.0))
                    .into(),
            };
            column![sticker, container(meta_row(t, msg)).align_right(Length::Fill)]
                .spacing(2)
                .width(w)
                .into()
        }
        Content::Voice {
            seconds, waveform, ..
        } => voice(app, msg, *seconds, waveform),
        Content::Unsupported(kind) => column![
            text(format!("This message type ({kind}) isn't supported yet. Open WhatsApp on your phone to view it."))
                .size(m.text)
                .color(p.text_secondary),
            container(meta_row(t, msg)).align_right(Length::Fill)
        ]
        .width(Length::Shrink)
        .into(),
        Content::Call(log) => super::call::log_body(app, msg, log, meta_row(t, msg)),
        Content::ViewOnce => row![
            icon(Icon::CircleDashed, 16.0, p.text_secondary),
            text("View once message: open it on your phone")
                .size(m.text)
                .color(p.text_secondary),
            gap(8.0),
            meta_row(t, msg),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
        Content::Deleted => row![
            icon(Icon::CircleAlert, 15.0, p.text_secondary),
            text(if out { "You deleted this message" } else { "This message was deleted" })
                .size(m.text)
                .color(p.text_secondary),
            gap(8.0),
            meta_row(t, msg),
        ]
        .spacing(5)
        .align_y(Alignment::Center)
        .into(),
    };
    finish(app, msg, first, col.push(body).into())
}

/// Wraps content in the bubble chrome, tail and alignment, plus the hover
/// actions, reactions badge and popups that belong to one message row.
fn finish<'a>(app: &'a App, msg: &'a ChatMessage, first: bool, content: El<'a>) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let out = msg.from_me;
    let color = if out { p.bubble_out } else { p.bubble_in };
    let sticker = matches!(msg.content, Content::Sticker { .. });
    let mut b = container(content)
        .padding(iced::Padding {
            top: 6.0,
            right: 8.0,
            bottom: 5.0,
            left: 9.0,
        })
        // Explicit Shrink: the content holds Fill children (right-aligned
        // time, full-width quote), and iced 0.14 makes a container with a
        // Fill child Fill itself. The bubble then shared the row 50/50 with
        // the spacer beside it: every bubble was capped at half the pane
        // (minus the hover slot), wrapped early and pushed the time into the
        // text. Shrink compresses the content to its natural width instead.
        .width(Length::Shrink)
        .max_width(t.metrics.bubble_max)
        .style(theme::bubble(t, out, first && !sticker));
    if matches!(msg.content, Content::Image { .. } | Content::Video { .. }) {
        b = b.padding(3);
    }
    if sticker {
        b = b.padding(2).style(theme::solid(Color::TRANSPARENT));
    }
    // The drop shadow: one plain quad 1 px lower (see `theme::bubble`).
    let b = if sticker {
        container(b)
    } else {
        container(b)
            .padding(iced::Padding {
                bottom: 1.0,
                ..Default::default()
            })
            .style(theme::bubble_shadow(t, out, first))
    };
    let bubble: El<'a> = if msg.status == ReceiptStatus::Failed && out {
        mouse_area(b)
            .on_press(Message::Ui(Ui::Retry(msg.id.clone())))
            .into()
    } else {
        b.into()
    };
    let tail_or_gap: El<'a> = if first && !sticker {
        tail(color, out)
    } else {
        gap(8.0)
    };
    let deleted = matches!(msg.content, Content::Deleted | Content::ViewOnce);
    let popup = app
        .popup
        .as_ref()
        .filter(|(id, _)| *id == msg.id)
        .map(|(_, w)| *w);
    let selecting = app.selection.as_ref().filter(|s| s.chat == msg.chat);
    let show_actions = selecting.is_none()
        && !deleted
        && (app.hovered.as_deref() == Some(msg.id.as_str()) || popup.is_some());
    let actions = action_slot(app, msg, show_actions, deleted);
    let line = if out {
        row![fill(), actions, bubble, tail_or_gap]
    } else {
        row![tail_or_gap, bubble, actions, fill()]
    };
    let mut stack = column![line.align_y(Alignment::Center)];
    if !deleted && !msg.reactions.is_empty() {
        let badge = reaction_badge(app, msg);
        stack = stack.push(if out {
            row![fill(), badge, gap(16.0)]
        } else {
            row![gap(20.0), badge, fill()]
        });
    }
    let selected = selecting.is_some_and(|s| s.contains(&msg.id));
    let highlighted = selected || app.highlight.as_ref().is_some_and(|(id, _)| *id == msg.id);
    let hl = p.accent.scale_alpha(if selected { 0.12 } else { 0.22 });
    let stack: El<'a> = if selecting.is_some() {
        // Selection mode: a checkbox in front of every row that can be
        // forwarded; the row itself is covered by a click catcher (below).
        let check: El<'a> = if msg.can_forward() {
            let id = msg.id.clone();
            checkbox(selected)
                .on_toggle(move |_| Message::Ui(Ui::Forward(Fwd::Toggle(id.clone()))))
                .size(18)
                .style(theme::checkbox(t))
                .into()
        } else {
            Space::new().into()
        };
        row![
            container(check)
                .width(SELECT_SLOT_W)
                .padding(iced::Padding {
                    left: 4.0,
                    ..Default::default()
                }),
            stack
        ]
        .align_y(Alignment::Center)
        .into()
    } else {
        stack.into()
    };
    let row_el = container(stack)
        .id(row_id(&msg.id))
        .width(Length::Fill)
        .padding(iced::Padding {
            top: if first { 6.0 } else { 0.0 },
            ..Default::default()
        })
        .style(move |_: &iced::Theme| iced::widget::container::Style {
            background: highlighted.then_some(iced::Background::Color(hl)),
            ..Default::default()
        });
    let id = msg.id.clone();
    if selecting.is_some() {
        // Any click on the row toggles it, even on a picture or a link.
        let catcher = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_press(Message::Ui(Ui::Forward(Fwd::Toggle(id))));
        return iced::widget::stack![row_el, catcher].into();
    }
    mouse_area(row_el)
        .on_enter(Message::Ui(Ui::HoverMessage(id.clone())))
        .on_exit(Message::Ui(Ui::UnhoverMessage(id.clone())))
        .on_right_press(Message::Ui(Ui::ToggleMessageMenu(id)))
        .into()
}

/// Width of the checkbox column in selection mode.
pub const SELECT_SLOT_W: f32 = 36.0;

/// Width of the slot reserved next to every bubble for the hover buttons.
pub const ACTION_SLOT_W: f32 = 2.0 * ACTION_BTN + 2.0 + 8.0;
/// Side of one hover button (icon plus padding).
pub const ACTION_BTN: f32 = 17.0 + 2.0 * 5.0;

/// The hover buttons' place beside a bubble. The slot has the same size
/// whether the buttons are shown or not: showing them on hover must never
/// change the row's layout, otherwise the row reflows under the pointer,
/// the pointer "leaves" and "re-enters" and the whole list flickers.
fn action_slot<'a>(app: &'a App, msg: &'a ChatMessage, show: bool, deleted: bool) -> El<'a> {
    if deleted {
        return gap(0.0);
    }
    let inner: El<'a> = if show {
        hover_actions(app, msg)
    } else {
        Space::new().into()
    };
    container(inner)
        .width(ACTION_SLOT_W)
        .height(ACTION_BTN)
        .align_y(Alignment::Center)
        .into()
}

/// Smiley and chevron buttons that appear next to a hovered bubble.
fn hover_actions<'a>(app: &'a App, msg: &'a ChatMessage) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let btn = |i: Icon, on: Message| -> El<'a> {
        button(icon(i, 17.0, p.text_secondary))
            .padding(5)
            .style(theme::icon_button(t, false))
            .on_press(on)
            .into()
    };
    container(
        row![
            btn(
                Icon::SmilePlus,
                Message::Ui(Ui::ToggleReactionPicker(msg.id.clone()))
            ),
            btn(
                Icon::ChevronDown,
                Message::Ui(Ui::ToggleMessageMenu(msg.id.clone()))
            ),
        ]
        .spacing(2)
        .align_y(Alignment::Center),
    )
    .padding([0, 4])
    .into()
}

/// Six quick reactions.
fn reaction_picker<'a>(app: &'a App, msg: &'a ChatMessage) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let mine = app
        .own_jid
        .as_deref()
        .and_then(|me| msg.reactions.iter().find(|r| r.reactor == me))
        .map(|r| r.emoji.as_str());
    let row_el = row(QUICK_REACTIONS.iter().map(|e| {
        let selected = mine == Some(*e);
        button(text(*e).size(24).font(emoji_font()))
            .padding([4, 6])
            .style(theme::icon_button(t, selected))
            .on_press(Message::Ui(Ui::React {
                id: msg.id.clone(),
                emoji: (*e).into(),
            }))
            .into()
    }))
    .spacing(2)
    .align_y(Alignment::Center);
    container(row_el)
        .padding([4, 8])
        .style(move |th: &iced::Theme| {
            let mut s = theme::rounded(p.bg_elevated, 26.0)(th);
            s.shadow = iced::Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
                offset: iced::Vector::new(0.0, 2.0),
                blur_radius: 8.0,
            };
            s
        })
        .into()
}

/// Reply / copy / edit / delete.
fn message_menu<'a>(app: &'a App, msg: &'a ChatMessage) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let now = chrono::Utc::now().timestamp();
    let item = |i: Icon, label: &'static str, on: Ui| -> El<'a> {
        button(
            row![
                icon(i, 17.0, p.text_secondary),
                text(label).size(14.5).color(p.text_primary)
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
    let id = msg.id.clone();
    let mut col = column![item(Icon::Reply, "Reply", Ui::StartReply(id.clone()))];
    col = col.push(item(
        Icon::SmilePlus,
        "React",
        Ui::ToggleReactionPicker(id.clone()),
    ));
    col = col.push(item(Icon::Copy, "Copy", Ui::CopyText(id.clone())));
    if msg.can_forward() {
        col = col.push(item(
            Icon::Forward,
            "Forward",
            Ui::Forward(Fwd::One(id.clone())),
        ));
    }
    if msg.from_me {
        col = col.push(item(
            Icon::Info,
            "Message info",
            Ui::MessageInfo(id.clone()),
        ));
    }
    let starred = app.is_starred(&msg.chat, &msg.id);
    col = col.push(item(
        if starred { Icon::StarOff } else { Icon::Star },
        if starred { "Unstar" } else { "Star" },
        Ui::Nav(crate::app::nav::NavMsg::ToggleStar(id.clone())),
    ));
    if matches!(msg.content, Content::Video { .. }) {
        col = col.push(item(
            Icon::ExternalLink,
            "Open externally",
            Ui::OpenVideoExternal(id.clone()),
        ));
    }
    if msg.can_edit(now) {
        col = col.push(item(Icon::Pencil, "Edit", Ui::StartEdit(id.clone())));
    }
    col = col.push(item(
        Icon::Check,
        "Select messages",
        Ui::Forward(Fwd::Select(id.clone())),
    ));
    col = col.push(item(Icon::Trash2, "Delete", Ui::AskDelete(id)));
    container(col.width(210))
        .padding([6, 0])
        .style(move |th: &iced::Theme| {
            let mut s = theme::rounded(p.bg_elevated, 8.0)(th);
            s.shadow = iced::Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
                offset: iced::Vector::new(0.0, 2.0),
                blur_radius: 10.0,
            };
            s
        })
        .into()
}

/// The reaction pill under a bubble; click shows who reacted.
fn reaction_badge<'a>(app: &'a App, msg: &'a ChatMessage) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let (counts, total) = reaction_summary(&msg.reactions);
    let mut r = row![].spacing(2).align_y(Alignment::Center);
    for (e, _) in counts.iter().take(3) {
        r = r.push(text(e.clone()).size(14).font(emoji_font()));
    }
    if total > 1 {
        r = r.push(text(total.to_string()).size(12).color(p.text_secondary));
    }
    let bg = p.bg_elevated;
    let edge = p.bg_panel;
    button(r)
        .padding([2, 7])
        .style(move |_: &iced::Theme, _| iced::widget::button::Style {
            background: Some(iced::Background::Color(bg)),
            text_color: Color::WHITE,
            border: iced::Border {
                radius: 11.0.into(),
                width: 2.0,
                color: edge,
            },
            ..Default::default()
        })
        .on_press(Message::Ui(Ui::ShowReactions(msg.id.clone())))
        .into()
}

/// The quoted message inside a reply bubble; click jumps to the original.
fn quote_block<'a>(app: &'a App, msg: &'a ChatMessage, q: &'a Quote) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let inset = if msg.from_me { p.inset_out } else { p.inset_in };
    let bar = quote_color(app, q);
    let inner = row![
        container(Space::new().width(4).height(Length::Fill)).style(theme::solid(bar)),
        column![
            text(quote_author(app, q))
                .size(13)
                .font(INTER_MEDIUM)
                .color(bar)
                .shaping(text::Shaping::Advanced),
            text(super::ellipsize(
                &app.chats.render_mentions(&q.preview, &[], &app.own_ids()),
                QUOTE_CHARS,
            ))
            .size(13.5)
            .color(if msg.from_me {
                p.bubble_out_meta
            } else {
                p.text_secondary
            })
            .wrapping(Wrapping::WordOrGlyph)
            .shaping(text::Shaping::Advanced),
        ]
        .spacing(2)
        .padding([6, 8]),
    ]
    .height(Length::Shrink);
    let block = button(inner)
        .padding(0)
        .width(Length::Fill)
        .style(move |th, st| {
            let mut s = theme::flat_button(p.text_primary)(th, st);
            s.background = Some(iced::Background::Color(inset));
            s.border.radius = 6.0.into();
            s
        })
        .on_press(Message::Ui(Ui::JumpTo(q.id.clone())));
    block.into()
}

/// The quote fills the bubble's width; this invisible, zero-height copy of
/// its one-line width (a direct child of the bubble column) makes the bubble
/// at least that wide, up to its maximum, so a short reply to a long message
/// does not squeeze the quote into a narrow column. Nothing is drawn outside
/// the bubble: the preview wraps instead of running past its edge.
fn quote_sizer<'a>(app: &'a App, q: &'a Quote) -> El<'a> {
    super::measure::width_of(
        column![
            text(quote_author(app, q))
                .size(13)
                .font(INTER_MEDIUM)
                .wrapping(Wrapping::None)
                .shaping(text::Shaping::Advanced),
            text(super::ellipsize(
                &app.chats.render_mentions(&q.preview, &[], &app.own_ids()),
                QUOTE_CHARS,
            ))
            .size(13.5)
            .wrapping(Wrapping::None)
            .shaping(text::Shaping::Advanced),
        ]
        .padding(iced::Padding {
            left: 4.0 + 8.0,
            right: 8.0 + 2.0,
            ..Default::default()
        }),
    )
}

/// A bubble picture. Pictures are drawn with `ContentFit::Contain`, never
/// `Cover`: iced draws a covering picture at its scaled size, and the
/// tiny-skia renderer does not clip images (its image path uses the clip
/// mask without fitting it to the clip bounds), so a thumbnail whose aspect
/// differed from its box (link previews, WhatsApp thumbnails) was painted
/// over the neighbouring bubbles: the dark rectangle over the previous
/// message's text and time. The clip stays for the wgpu renderer.
fn cover<'a>(img: impl Into<El<'a>>) -> El<'a> {
    container(img).clip(true).into()
}

/// Characters of a quoted message shown in a reply bubble (it wraps).
const QUOTE_CHARS: usize = 140;

/// `https://www.example.com/a/b` -> `example.com`.
fn link_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#', ':']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// Message text: links are clickable, mentions show names (highlighted),
/// emoji are drawn in colour and the active in-chat search term is
/// highlighted.
fn message_text<'a>(app: &'a App, msg: &'a ChatMessage, s: &'a str, fg: Color) -> El<'a> {
    use crate::app::linkify::{self, Token};
    use crate::ui::emoji_font::{emoji_spans, emoji_text, has_emoji};
    use iced::widget::span;
    let m = app.theme.metrics;
    let p = app.theme.palette;
    let term = app
        .chat_search
        .as_ref()
        .map(|c| c.query.trim().to_lowercase())
        .filter(|q| q.chars().count() >= 2);
    let tokens = linkify::tokenize(s);
    if tokens.iter().all(|t| matches!(t, Token::Text(_))) {
        if let Some(term) = term {
            let spans = highlight_spans(s, &term, p.notice_text);
            if spans.len() > 1 {
                return iced::widget::rich_text::<(), Message, iced::Theme, iced::Renderer>(spans)
                    .size(m.text)
                    .wrapping(Wrapping::WordOrGlyph)
                    .color(fg)
                    .into();
            }
        }
        if has_emoji(s) {
            return emoji_text(s, m.text, fg, theme::INTER, true);
        }
        return text(s)
            .size(m.text)
            .wrapping(Wrapping::WordOrGlyph)
            .color(fg)
            .shaping(text::Shaping::Advanced)
            .into();
    }
    let own: Vec<&str> = app.own_jid.as_deref().into_iter().collect();
    let mut spans: Vec<iced::widget::text::Span<'a, String, iced::Font>> = Vec::new();
    for tok in tokens {
        match tok {
            Token::Text(x) => match &term {
                Some(term) => spans.extend(highlight_generic(x, term, p.notice_text)),
                None => spans.extend(emoji_spans(x, |sp| sp)),
            },
            Token::Url { text, href } | Token::Email { text, href } => {
                spans.push(span(text).color(p.link).underline(true).link(href));
            }
            Token::Mention { user, .. } => {
                let name = app.chats.mention_label(user, &msg.mentions, &own);
                spans.extend(emoji_spans(&format!("@{name}"), |sp| sp.color(p.link)));
            }
        }
    }
    iced::widget::rich_text::<String, Message, iced::Theme, iced::Renderer>(spans)
        .on_link_click(|href| Message::Ui(Ui::OpenLink(href)))
        .size(m.text)
        .wrapping(Wrapping::WordOrGlyph)
        .color(fg)
        .into()
}

/// Splits `s` into plain and highlighted spans around case-insensitive
/// matches of `term` (already lower-cased).
pub fn highlight_spans<'a>(
    s: &'a str,
    term: &str,
    bg: Color,
) -> Vec<iced::widget::text::Span<'a, (), iced::Font>> {
    highlight_generic(s, term, bg)
}

fn highlight_generic<'a, L>(
    s: &'a str,
    term: &str,
    bg: Color,
) -> Vec<iced::widget::text::Span<'a, L, iced::Font>> {
    use iced::widget::span;
    let lower = s.to_lowercase();
    // Lower-casing can change byte lengths; only highlight when it did not.
    if lower.len() != s.len() || term.is_empty() {
        return vec![span(s)];
    }
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(pos) = lower.get(at..).and_then(|rest| rest.find(term)) {
        let (start, end) = (at + pos, at + pos + term.len());
        if !s.is_char_boundary(start) || !s.is_char_boundary(end) {
            break;
        }
        if start > at {
            spans.push(span(&s[at..start]));
        }
        spans.push(span(&s[start..end]).background(bg).color(Color::BLACK));
        at = end;
    }
    if at < s.len() {
        spans.push(span(&s[at..]));
    }
    if spans.is_empty() {
        spans.push(span(s));
    }
    spans
}

/// Modal over the conversation: delete confirmation or reaction list.
pub fn dialog<'a>(app: &'a App) -> Option<El<'a>> {
    let t = &app.theme;
    let p = t.palette;
    let d = app.dialog.as_ref()?;
    let msg = app.dialog_message()?;
    let now = chrono::Utc::now().timestamp();
    let body: El<'a> = match d {
        Dialog::Delete(_) => {
            let btn = |label: &'static str, on: Message, danger: bool| -> El<'a> {
                button(text(label).size(14.5).font(INTER_MEDIUM).color(if danger {
                    p.danger
                } else {
                    p.accent
                }))
                .padding([9, 16])
                .style(theme::link_button(t))
                .on_press(on)
                .into()
            };
            let mut col = column![
                text("Delete message?").size(18).color(p.text_primary),
                text(if msg.can_revoke(now) {
                    "You can delete this message for everyone or just for you."
                } else {
                    "This message will be deleted only on this device."
                })
                .size(14)
                .color(p.text_secondary),
            ]
            .spacing(10);
            let mut buttons = column![]
                .spacing(2)
                .align_x(Alignment::End)
                .width(Length::Fill);
            if msg.can_revoke(now) {
                buttons = buttons.push(btn(
                    "Delete for everyone",
                    Message::Ui(Ui::ConfirmDelete {
                        id: msg.id.clone(),
                        everyone: true,
                    }),
                    true,
                ));
            }
            buttons = buttons
                .push(btn(
                    "Delete for me",
                    Message::Ui(Ui::ConfirmDelete {
                        id: msg.id.clone(),
                        everyone: false,
                    }),
                    true,
                ))
                .push(btn("Cancel", Message::Ui(Ui::CloseDialog), false));
            col = col.push(buttons);
            col.into()
        }
        Dialog::Reactions(_) => {
            let mut col = column![
                text(format!("{} reactions", msg.reactions.len()))
                    .size(18)
                    .color(p.text_primary)
            ]
            .spacing(12);
            for r in &msg.reactions {
                let me = app.own_jid.as_deref() == Some(r.reactor.as_str());
                let name = if me {
                    "You".to_string()
                } else {
                    app.chats.display_name(&r.reactor)
                };
                col = col.push(
                    row![
                        avatar_for(app, &r.reactor, false, 36.0),
                        container(emoji_text(name, 15.0, p.text_primary, theme::INTER, false))
                            .width(Length::Fill),
                        text(r.emoji.as_str()).size(22).font(emoji_font()),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                );
            }
            col = col.push(
                container(
                    button(text("Close").size(14.5).font(INTER_MEDIUM).color(p.accent))
                        .padding([9, 16])
                        .style(theme::link_button(t))
                        .on_press(Message::Ui(Ui::CloseDialog)),
                )
                .align_right(Length::Fill),
            );
            col.into()
        }
    };
    let card = container(body)
        .padding(24)
        .width(380)
        .style(move |th: &iced::Theme| {
            let mut s = theme::rounded(p.bg_elevated, 12.0)(th);
            s.shadow = iced::Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                offset: iced::Vector::new(0.0, 4.0),
                blur_radius: 24.0,
            };
            s
        });
    Some(opaque(
        container(card)
            .center(Length::Fill)
            .style(theme::solid(Color::from_rgba(0.0, 0.0, 0.0, 0.55))),
    ))
}

fn voice<'a>(app: &'a App, msg: &'a ChatMessage, seconds: u32, wf: &'a [u8]) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let out = msg.from_me;
    let state = app.voice.as_ref().filter(|v| v.id == msg.id);
    let playing = state.is_some_and(|v| !v.paused && !v.loading);
    let loading = state.is_some_and(|v| v.loading);
    let progress = state
        .filter(|v| v.duration_ms > 0)
        .map(|v| v.position_ms as f32 / v.duration_ms as f32)
        .unwrap_or(0.0);

    let btn_icon = if playing { Icon::Pause } else { Icon::Play };
    let play = button(icon(btn_icon, 22.0, p.text_secondary))
        .padding(6)
        .style(theme::flat_button(p.text_secondary))
        .on_press(Message::Ui(if playing {
            Ui::PauseVoice
        } else {
            Ui::PlayVoice(msg.id.clone())
        }));

    // 40 bars resampled from the 64-sample waveform.
    const BARS: usize = 40;
    let samples: Vec<u8> = if wf.is_empty() {
        vec![20; BARS]
    } else {
        (0..BARS).map(|i| wf[i * wf.len() / BARS]).collect()
    };
    let played_bars = (progress * BARS as f32).round() as usize;
    let played_color = if out {
        Color::from_rgb8(0xe9, 0xed, 0xef)
    } else {
        p.success
    };
    let idle_color = if out { p.bubble_out_meta } else { p.text_muted };
    let bars = row(samples.iter().enumerate().map(|(i, v)| {
        let h = 3.0 + (*v as f32 / 100.0) * 22.0;
        container(Space::new().width(2.5).height(h))
            .style(theme::rounded(
                if i < played_bars {
                    played_color
                } else {
                    idle_color
                },
                1.5,
            ))
            .into()
    }))
    .spacing(1.5)
    .align_y(Alignment::Center)
    .height(26);

    let left = state
        .filter(|v| v.position_ms > 0)
        .map(|v| format_duration((v.position_ms / 1000) as u32))
        .unwrap_or_else(|| format_duration(seconds));
    let meta_color = if out {
        p.bubble_out_meta
    } else {
        p.text_secondary
    };
    let mic_badge = container(icon(
        Icon::Mic,
        14.0,
        if msg.status == ReceiptStatus::Played || !out {
            p.success
        } else {
            p.text_secondary
        },
    ))
    .padding(2);
    let avatar_part: El<'a> = iced::widget::stack![
        avatar_for(
            app,
            if out {
                app.own_jid.as_deref().unwrap_or_default()
            } else {
                &msg.sender
            },
            false,
            46.0
        ),
        container(mic_badge)
            .width(46)
            .height(46)
            .align_x(Alignment::End)
            .align_y(Alignment::End)
    ]
    .into();

    let player = column![
        row![play, bars].spacing(4).align_y(Alignment::Center),
        row![
            gap(38.0),
            text(if loading {
                "Loading…".to_string()
            } else {
                left
            })
            .size(t.metrics.text_tiny)
            .color(meta_color),
            fill(),
            meta_row(t, msg)
        ]
        .align_y(Alignment::Center),
    ]
    .spacing(0)
    .width(280);

    if out {
        row![player, avatar_part]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
    } else {
        row![avatar_part, player]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
    }
}

fn doc_color(ext: &str) -> Color {
    match ext {
        "PDF" => Color::from_rgb8(0xe3, 0x4c, 0x3d),
        "DOC" | "DOCX" | "ODT" => Color::from_rgb8(0x3d, 0x7b, 0xe3),
        "XLS" | "XLSX" | "ODS" | "CSV" => Color::from_rgb8(0x2f, 0xa5, 0x5d),
        _ => Color::from_rgb8(0x79, 0x86, 0x8f),
    }
}

/// Scales `(w, h)` to fit in `(max_w, max_h)`, keeping a sane minimum.
pub fn fit(w: u32, h: u32, max_w: f32, max_h: f32) -> (f32, f32) {
    if w == 0 || h == 0 {
        return (max_w, max_w * 0.66);
    }
    let (w, h) = (w as f32, h as f32);
    let s = (max_w / w).min(max_h / h).clamp(0.01, 1.0);
    ((w * s).max(120.0), (h * s).max(80.0))
}

/// The in-app video player: picture, play / pause, seek bar with times,
/// volume, full screen, "Open externally" and close.
pub fn video_viewer<'a>(app: &'a App, v: &'a crate::app::video::VideoView) -> El<'a> {
    use crate::app::video::VideoPhase;
    use crate::media::video::format_clock;
    use crate::media::video_proto::PlayState;
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let white = Color::WHITE;
    let dim = Color::from_rgba(1.0, 1.0, 1.0, 0.75);

    let ctl = |i: Icon, size: f32, on: Message| -> El<'a> {
        button(icon(i, size, white))
            .padding(8)
            .style(theme::icon_button(t, false))
            .on_press(on)
            .into()
    };

    let top = row![
        ctl(Icon::X, 22.0, Message::Ui(Ui::CloseVideo)),
        text("Video").size(16).color(white),
        fill(),
        button(
            row![
                icon(Icon::ExternalLink, 16.0, white),
                text("Open externally").size(13.5).color(white),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding([7, 12])
        .style(theme::icon_button(t, false))
        .on_press(Message::Ui(Ui::OpenVideoExternal(v.id.clone()))),
        ctl(
            if app.video_fullscreen {
                Icon::Minimize
            } else {
                Icon::Maximize
            },
            20.0,
            Message::Ui(Ui::VideoFullscreen)
        ),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .padding([8, 14]);

    let thumb = app.thumbs.get(&v.id).cloned();
    let picture: El<'a> = match (&v.frame, &v.phase) {
        (Some(h), _) => mouse_area(
            image(h.clone())
                .content_fit(ContentFit::Contain)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_press(Message::Ui(Ui::VideoToggle))
        .into(),
        (None, VideoPhase::Failed(msg)) => center(
            column![
                icon(Icon::CircleAlert, 40.0, p.notice_text),
                text(msg.as_str())
                    .size(15)
                    .color(white)
                    .align_x(Alignment::Center)
                    .width(480),
                row![
                    button(text("Open externally").size(14.5).font(INTER_MEDIUM))
                        .padding([9, 16])
                        .style(theme::chip(t, true))
                        .on_press(Message::Ui(Ui::OpenVideoExternal(v.id.clone()))),
                    button(text("Close").size(14.5).font(INTER_MEDIUM))
                        .padding([9, 16])
                        .style(theme::chip(t, false))
                        .on_press(Message::Ui(Ui::CloseVideo)),
                ]
                .spacing(10),
            ]
            .spacing(16)
            .align_x(Alignment::Center),
        )
        .into(),
        (None, phase) => {
            let label = if *phase == VideoPhase::Downloading {
                "Downloading video…"
            } else {
                "Starting playback…"
            };
            let backdrop: El<'a> = match thumb {
                Some(h) => image(h)
                    .content_fit(ContentFit::Contain)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .opacity(0.45_f32)
                    .into(),
                None => Space::new().into(),
            };
            iced::widget::stack![backdrop, center(text(label).size(16).color(white))].into()
        }
    };

    let playing = v.is_playing();
    let ready = v.phase == VideoPhase::Playing;
    let play_icon = if v.state == PlayState::Ended {
        Icon::RotateCcw
    } else if playing {
        Icon::Pause
    } else {
        Icon::Play
    };
    let mut play = button(icon(play_icon, 22.0, white))
        .padding(8)
        .style(theme::icon_button(t, false));
    if ready {
        play = play.on_press(Message::Ui(Ui::VideoToggle));
    }
    let mut seek = iced::widget::slider(0.0..=1.0, v.progress(), |f| Message::Ui(Ui::VideoSeek(f)))
        .step(0.001_f32)
        .on_release(Message::Ui(Ui::VideoSeekRelease));
    if !ready || v.duration_ms == 0 {
        seek = iced::widget::slider(0.0..=1.0, 0.0, |_| Message::Ui(Ui::VideoSeek(0.0)));
    }
    let shown_ms = v.seeking.map_or(v.position_ms, |f| {
        crate::media::video::seek_target(f, v.duration_ms)
    });
    let vol_icon = if v.muted || v.volume == 0 {
        Icon::VolumeX
    } else {
        Icon::Volume2
    };
    let controls = container(
        row![
            play,
            text(format_clock(shown_ms)).size(13).color(dim),
            seek.width(Length::Fill),
            text(if v.duration_ms > 0 {
                format_clock(v.duration_ms)
            } else {
                "--:--".into()
            })
            .size(13)
            .color(dim),
            ctl(vol_icon, 20.0, Message::Ui(Ui::VideoMute)),
            iced::widget::slider(0..=100u8, if v.muted { 0 } else { v.volume }, |x| {
                Message::Ui(Ui::VideoVolume(x))
            })
            .width(90),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([8, 18]);

    let _ = m;
    opaque(
        container(column![
            top,
            container(picture)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding([0, 24]),
            controls,
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::solid(Color::from_rgb(0.04, 0.05, 0.06))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_spans_split_around_matches() {
        let spans = highlight_spans("Hola hola HOLA", "hola", Color::WHITE);
        // plain, match, plain, match, plain, match => 3 matches + 2 gaps
        assert_eq!(spans.len(), 5);
        assert_eq!(
            highlight_spans("nothing here", "zzz", Color::WHITE).len(),
            1
        );
        // Characters whose lower-case form has a different length are never
        // sliced at a wrong boundary.
        assert_eq!(highlight_spans("İstanbul", "stan", Color::WHITE).len(), 1);
        assert_eq!(highlight_spans("", "x", Color::WHITE).len(), 1);
        assert_eq!(highlight_spans("ñandú ñ", "ñ", Color::WHITE).len(), 3);
    }

    #[test]
    fn fit_keeps_aspect_and_bounds() {
        let (w, h) = fit(1280, 640, 330.0, 330.0);
        assert!((w - 330.0).abs() < 0.1);
        assert!((h - 165.0).abs() < 0.1);
        let (w, _) = fit(10, 10, 330.0, 330.0);
        assert_eq!(w, 120.0);
        assert_eq!(fit(0, 0, 300.0, 300.0).0, 300.0);
    }
}

/// Layout regression tests: hovering a message must not change the size of
/// its row. A row that grows when the hover buttons appear reflows under the
/// pointer, which fires "pointer left" and "pointer entered" in a loop: that
/// was the flicker reported on the message list.
#[cfg(test)]
mod layout_tests {
    use super::*;
    use crate::config::Config;
    use iced::advanced::layout::Limits;
    use iced::advanced::renderer::Headless;
    use iced::advanced::widget::Tree;
    use iced::{Pixels, Size};

    fn app() -> App {
        let mut cfg = Config::defaults();
        cfg.mock = true;
        App::boot(cfg).0
    }

    fn size_of(mut el: El<'_>, width: f32) -> Size {
        let renderer = futures::executor::block_on(<iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("tiny-skia renderer");
        let mut tree = Tree::new(el.as_widget());
        el.as_widget_mut()
            .layout(
                &mut tree,
                &renderer,
                &Limits::new(Size::ZERO, Size::new(width, 5000.0)),
            )
            .size()
    }

    fn msg(id: &str, from_me: bool, text: &str) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            chat: "34611111111@s.whatsapp.net".into(),
            sender: "34611111111@s.whatsapp.net".into(),
            sender_name: None,
            from_me,
            timestamp: 1_700_000_000,
            content: Content::Text(text.into()),
            status: ReceiptStatus::Read,
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: Vec::new(),
            reactions: Vec::new(),
        }
    }

    #[test]
    fn hover_buttons_never_change_the_row_size() {
        let mut app = app();
        let long = "una frase larga que ocupa varias líneas del globo de mensaje ".repeat(12);
        let cases = [
            msg("short-in", false, "ok"),
            msg("short-out", true, "ok"),
            msg("long-in", false, &long),
            msg("long-out", true, &long),
        ];
        // Wide, typical and squeezed panes: the squeezed one is where the
        // buttons used to steal width from a full-width bubble.
        for width in [1000.0, 700.0, 460.0] {
            for m in &cases {
                app.hovered = None;
                let idle = size_of(bubble(&app, m, true, false), width);
                app.hovered = Some(m.id.clone());
                let hovered = size_of(bubble(&app, m, true, false), width);
                assert_eq!(
                    idle, hovered,
                    "row {} reflowed on hover at width {width}",
                    m.id
                );
            }
        }
    }

    #[test]
    fn an_open_popup_does_not_change_the_row_size_either() {
        let mut app = app();
        let m = msg("p", true, "ok");
        app.popup = None;
        let idle = size_of(bubble(&app, &m, true, false), 800.0);
        app.popup = Some(("p".into(), Popup::Menu));
        let open = size_of(bubble(&app, &m, true, false), 800.0);
        assert_eq!(idle, open);
    }

    fn layout_of(el: &mut El<'_>, width: f32) -> iced::advanced::layout::Node {
        let renderer = futures::executor::block_on(<iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("tiny-skia renderer");
        let mut tree = Tree::new(el.as_widget());
        el.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, Size::new(width, 5000.0)),
        )
    }

    /// The bubble node inside a row laid out by `finish` (row -> column ->
    /// line row -> [tail, bubble, slot, spacer] for incoming messages).
    fn bubble_width(node: &iced::advanced::layout::Node, out: bool) -> f32 {
        let line = &node.children()[0].children()[0];
        let kids = line.children();
        // Outgoing: [spacer, slot, bubble, tail].
        let i = if out { 2 } else { 1 };
        kids[i].bounds().width
    }

    /// Every descendant stays inside its parent: nothing is drawn over the
    /// bubble edge or the time.
    fn assert_contained(n: &iced::advanced::layout::Node, depth: usize) {
        let b = n.bounds();
        for c in n.children() {
            let cb = c.bounds();
            assert!(
                cb.x >= -0.5 && cb.y >= -0.5 && cb.x + cb.width <= b.width + 0.5,
                "child {cb:?} escapes parent {b:?} at depth {depth}"
            );
            assert_contained(c, depth + 1);
        }
    }

    /// Round-4 regression: a Fill child (the right-aligned time) made the
    /// bubble Fill too, so it shared the row 50/50 with the spacer and every
    /// bubble was capped at half the pane.
    #[test]
    fn bubbles_take_their_natural_width_not_half_the_pane() {
        let app = app();
        let long = "Mira esto que es un texto bastante largo y también otra cosa más para ver";
        for (out, width) in [
            (false, 684.0),
            (true, 684.0),
            (false, 900.0),
            (true, 1200.0),
        ] {
            let m = msg("w", out, long);
            let mut el = bubble(&app, &m, true, false);
            let node = layout_of(&mut el, width);
            let w = bubble_width(&node, out);
            // Natural one/two-line width up to the 520 px maximum, never
            // half of the row.
            assert!(w > 470.0 && w <= 520.0, "bubble {w} px at pane {width}");
            assert_contained(&node, 0);
        }
        // Short text stays short.
        let m = msg("s", false, "ok");
        let mut el = bubble(&app, &m, true, false);
        let w = bubble_width(&layout_of(&mut el, 900.0), false);
        assert!(w < 120.0, "short bubble {w}");
    }

    #[test]
    fn a_short_reply_is_as_wide_as_its_quote_and_nothing_overflows() {
        let app = app();
        let mut m = msg("r", false, "vale");
        m.reply = Some(Quote {
            id: "q".into(),
            sender: "34611111111@s.whatsapp.net".into(),
            preview: "Nos vemos mañana en la estación, llevo yo el café y los bollos".into(),
        });
        let mut el = bubble(&app, &m, true, false);
        let node = layout_of(&mut el, 900.0);
        let w = bubble_width(&node, false);
        assert!(w > 300.0, "the quote widened the bubble to {w}");
        assert_contained(&node, 0);
        // A very long unbreakable word wraps inside the bubble.
        let m = msg("u", true, &"x".repeat(400));
        let mut el = bubble(&app, &m, true, false);
        let node = layout_of(&mut el, 700.0);
        assert!(bubble_width(&node, true) <= 520.0);
        assert_contained(&node, 0);
    }

    #[test]
    fn action_slot_fits_its_buttons() {
        // Two buttons, the gap between them and the container padding.
        const { assert!(ACTION_SLOT_W >= 2.0 * ACTION_BTN + 2.0 + 8.0) };
    }

    #[test]
    fn icon_handles_are_created_once() {
        // A handle built on every `view` call would be re-hashed (and, with a
        // unique id, re-uploaded) on every redraw.
        let a = Icon::Play.handle();
        let b = Icon::Play.handle();
        assert_eq!(a.id(), b.id());
    }
}
