//! The contact / group info drawer (`app::info`) on the right of the
//! conversation, like WhatsApp Desktop's.

use iced::widget::text::Wrapping;
use iced::widget::{Space, button, column, container, image, row, scrollable, text, toggler};
use iced::{Alignment, Color, ContentFit, Length};

use super::emoji_font::emoji_text;
use super::icons::Icon;
use super::theme::{self, INTER, INTER_MEDIUM};
use super::{El, avatar_for, ellipsize, fill, icon};
use crate::app::info::{InfoPanel, Item, Load, STARRED_SHOWN, Tab};
use crate::app::{App, Message, Ui};
use crate::net::{Content, fallback_name, is_group_jid};

/// Width of the drawer.
pub const WIDTH: f32 = 380.0;
/// Side of a gallery thumbnail.
const THUMB: f32 = 82.0;

fn section<'a>(app: &'a App, body: El<'a>) -> El<'a> {
    container(body)
        .padding([14, 24])
        .width(Length::Fill)
        .style(theme::solid(app.theme.palette.bg_elevated))
        .into()
}

fn label<'a>(app: &'a App, s: impl Into<String>) -> El<'a> {
    text(s.into())
        .size(13.5)
        .color(app.theme.palette.text_secondary)
        .into()
}

/// The phone number of a person (never a raw LID).
fn phone_of(app: &App, jid: &str) -> Option<String> {
    let pn = if jid.ends_with("@s.whatsapp.net") {
        Some(jid.to_string())
    } else {
        app.chats
            .names
            .alias_of(jid)
            .filter(|a| a.ends_with("@s.whatsapp.net"))
            .map(str::to_string)
    };
    pn.map(|p| fallback_name(&p))
}

fn thumb_tile<'a>(app: &'a App, it: &'a Item) -> El<'a> {
    let p = app.theme.palette;
    let pic: El<'a> = match &it.thumb {
        // Contain, not Cover: tiny-skia does not clip images, a covering
        // picture would paint over its neighbours.
        Some(h) => container(
            image(h.clone())
                .width(THUMB)
                .height(THUMB)
                .content_fit(ContentFit::Contain),
        )
        .style(theme::rounded(p.inset_in, 4.0))
        .into(),
        None => container(icon(Icon::Image, 28.0, p.text_secondary))
            .center_x(THUMB)
            .center_y(THUMB)
            .style(theme::rounded(p.inset_in, 4.0))
            .into(),
    };
    let video = matches!(it.message.content, Content::Video { .. });
    let pic: El<'a> = if video {
        iced::widget::stack![
            pic,
            container(icon(Icon::Play, 20.0, Color::WHITE)).center(THUMB)
        ]
        .into()
    } else {
        pic
    };
    let on = if video {
        Ui::OpenVideo(it.message.id.clone())
    } else {
        Ui::JumpTo(it.message.id.clone())
    };
    let on = if matches!(it.message.content, Content::Image { .. }) {
        Ui::OpenImage(it.message.id.clone())
    } else {
        on
    };
    button(container(pic).width(THUMB).height(THUMB))
        .padding(0)
        .style(theme::flat_button(p.text_primary))
        .on_press(Message::Ui(on))
        .into()
}

fn list_row<'a>(app: &'a App, it: &'a Item) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let (title, sub, on) = match &it.message.content {
        Content::Document {
            file_name, size, ..
        } => (
            file_name.clone(),
            crate::media::image::human_size(*size),
            Ui::OpenDocument(it.message.id.clone()),
        ),
        Content::Link { preview, .. } => (
            preview.title.clone(),
            preview.url.clone(),
            Ui::OpenLink(preview.url.clone()),
        ),
        _ => (
            it.message.content.preview(),
            String::new(),
            Ui::JumpTo(it.message.id.clone()),
        ),
    };
    let lead: El<'a> = match &it.thumb {
        Some(h) => container(
            image(h.clone())
                .width(44)
                .height(44)
                .content_fit(ContentFit::Contain),
        )
        .into(),
        None => container(icon(Icon::FileText, 24.0, p.text_secondary))
            .center_x(44)
            .center_y(44)
            .style(theme::rounded(p.inset_in, 6.0))
            .into(),
    };
    button(
        row![
            lead,
            column![
                text(ellipsize(&title, 40))
                    .size(14.5)
                    .color(p.text_primary)
                    .wrapping(Wrapping::None),
                text(ellipsize(&sub, 48))
                    .size(12.5)
                    .color(p.text_secondary)
                    .wrapping(Wrapping::None),
            ]
            .spacing(2),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([8, 24])
    .width(Length::Fill)
    .style(theme::chat_row(t, false))
    .on_press(Message::Ui(on))
    .into()
}

/// The gallery page (replaces the drawer body).
fn gallery<'a>(app: &'a App, tab: Tab, items: &'a [Item]) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let tab_btn =
        |label: &'static str, which: Tab| -> El<'a> {
            let on = which == tab;
            button(
                column![
                    text(label).size(14).font(INTER_MEDIUM).color(if on {
                        p.accent
                    } else {
                        p.text_secondary
                    }),
                    container(Space::new().height(3).width(Length::Fill))
                        .style(theme::solid(if on { p.accent } else { Color::TRANSPARENT })),
                ]
                .spacing(8)
                .align_x(Alignment::Center),
            )
            .padding([10, 0])
            .width(Length::Fill)
            .style(theme::flat_button(p.text_primary))
            .on_press(Message::Ui(Ui::InfoGallery(Some(which))))
            .into()
        };
    let tabs = row![
        tab_btn("Media", Tab::Media),
        tab_btn("Docs", Tab::Docs),
        tab_btn("Links", Tab::Links)
    ];
    let body: El<'a> = if items.is_empty() {
        container(label(
            app,
            match tab {
                Tab::Media => "No media",
                Tab::Docs => "No documents",
                Tab::Links => "No links",
            },
        ))
        .padding(32)
        .center_x(Length::Fill)
        .into()
    } else if tab == Tab::Media {
        let mut col = column![].spacing(4);
        for chunk in items.chunks(4) {
            col = col.push(row(chunk.iter().map(|it| thumb_tile(app, it))).spacing(4));
        }
        container(col).padding([12, 16]).into()
    } else {
        column(items.iter().map(|it| list_row(app, it))).into()
    };
    column![
        tabs,
        scrollable(body)
            .direction(super::thin_scrollbar())
            .style(theme::scrollbar(t))
    ]
    .into()
}

/// The drawer for the open chat, if open.
pub fn view<'a>(app: &'a App, info: &'a InfoPanel) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let group = is_group_jid(&info.chat);
    let in_gallery = info.gallery.is_some();
    let close = if in_gallery {
        button(icon(Icon::ArrowLeft, 22.0, p.text_secondary))
            .padding(8)
            .style(theme::icon_button(t, false))
            .on_press(Message::Ui(Ui::InfoGallery(None)))
    } else {
        button(icon(Icon::X, 22.0, p.text_secondary))
            .padding(8)
            .style(theme::icon_button(t, false))
            .on_press(Message::Ui(Ui::CloseInfo))
    };
    let title = if in_gallery {
        "Media, links and docs"
    } else if group {
        "Group info"
    } else {
        "Contact info"
    };
    let header = container(
        row![close, text(title).size(16).color(p.text_primary)]
            .spacing(16)
            .align_y(Alignment::Center),
    )
    .height(m.header_height)
    .padding([0, 16])
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .style(theme::solid(p.bg_elevated));

    if let Some((tab, items)) = &info.gallery {
        return frame(app, column![header, gallery(app, *tab, items)].into());
    }

    let name = app.chats.display_name(&info.chat);
    let mut body = column![].spacing(10).width(Length::Fill);

    // Picture, name, number / member count.
    let subtitle = if group {
        match &info.group {
            Load::Ready(g) => format!("Group · {} members", g.participants.len()),
            Load::Loading => "Group".into(),
            Load::Unavailable => "Group".into(),
        }
    } else {
        phone_of(app, &info.chat).unwrap_or_default()
    };
    body = body.push(
        container(
            column![
                avatar_for(app, &info.chat, group, 200.0),
                emoji_text(name.clone(), 22.0, p.text_primary, INTER, true),
                text(subtitle).size(15).color(p.text_secondary),
            ]
            .spacing(10)
            .align_x(Alignment::Center),
        )
        .padding([28, 24])
        .center_x(Length::Fill)
        .style(theme::solid(p.bg_elevated)),
    );

    // About / description.
    if group {
        let desc: El<'a> = match &info.group {
            Load::Ready(g) => match &g.description {
                Some(d) => emoji_text(d.clone(), 14.5, p.text_primary, INTER, true),
                None => label(app, "No group description"),
            },
            Load::Loading => label(app, "Loading…"),
            Load::Unavailable => label(app, "Group info is not available."),
        };
        body = body.push(section(
            app,
            column![label(app, "Description"), desc].spacing(8).into(),
        ));
    } else {
        let mut about = column![label(app, "About")].spacing(8);
        about = about.push(match &info.contact {
            Load::Ready(c) => match &c.about {
                Some(a) => emoji_text(a.clone(), 14.5, p.text_primary, INTER, true),
                None => label(app, "Not available"),
            },
            Load::Loading => label(app, "Loading…"),
            Load::Unavailable => label(app, "Not available"),
        });
        if let Some(b) = info.contact.ready().and_then(|c| c.business_name.clone()) {
            about = about.push(label(app, format!("Business account · {b}")));
        }
        body = body.push(section(app, about.into()));
    }

    // Media, links and docs.
    let (media, links, docs) = info.counts;
    let total = media + links + docs;
    let mut media_col = column![
        button(
            row![
                label(app, "Media, links and docs"),
                fill(),
                label(app, total.to_string()),
                icon(Icon::ChevronRight, 18.0, p.text_secondary),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding(0)
        .width(Length::Fill)
        .style(theme::flat_button(p.text_primary))
        .on_press(Message::Ui(Ui::InfoGallery(Some(Tab::Media))))
    ]
    .spacing(12);
    if !info.recent.is_empty() {
        media_col =
            media_col.push(row(info.recent.iter().map(|it| thumb_tile(app, it))).spacing(6));
    }
    body = body.push(section(app, media_col.into()));

    // Starred messages of this chat.
    let mut starred = column![
        row![
            icon(Icon::Star, 18.0, p.text_secondary),
            text("Starred messages").size(15).color(p.text_primary),
            fill(),
            label(app, info.starred.len().to_string()),
        ]
        .spacing(16)
        .align_y(Alignment::Center)
    ]
    .spacing(8);
    for s in info.starred.iter().take(STARRED_SHOWN) {
        starred = starred.push(
            button(
                text(ellipsize(&app.preview(s), 60))
                    .size(13.5)
                    .color(p.text_secondary)
                    .wrapping(Wrapping::None),
            )
            .padding([2, 34])
            .style(theme::flat_button(p.text_primary))
            .on_press(Message::Ui(Ui::JumpTo(s.id.clone()))),
        );
    }
    body = body.push(section(app, starred.into()));

    // Mute.
    let muted = app.is_muted(&info.chat);
    let chat = info.chat.clone();
    body = body.push(section(
        app,
        row![
            icon(Icon::BellOff, 18.0, p.text_secondary),
            text("Mute notifications").size(15).color(p.text_primary),
            fill(),
            toggler(muted)
                .on_toggle(move |_| Message::Ui(Ui::ToggleMute(chat.clone())))
                .style(theme::toggler(t))
                .size(20),
        ]
        .spacing(16)
        .align_y(Alignment::Center)
        .into(),
    ));

    if group {
        // Members, admins first.
        let mut members = column![].spacing(2);
        match &info.group {
            Load::Ready(g) => {
                members = members.push(label(app, format!("{} members", g.participants.len())));
                let own = app.own_ids();
                for part in &g.participants {
                    let me = own.contains(&part.jid.as_str())
                        || part.alias.as_deref().is_some_and(|a| own.contains(&a));
                    let who = if me {
                        "You".to_string()
                    } else {
                        app.chats.display_name(&part.jid)
                    };
                    let mut line = row![
                        avatar_for(app, &part.jid, false, 40.0),
                        column![
                            emoji_text(ellipsize(&who, 30), 15.0, p.text_primary, INTER, false),
                            text(if me {
                                String::new()
                            } else {
                                phone_of(app, &part.jid).unwrap_or_default()
                            })
                            .size(12.5)
                            .color(p.text_secondary),
                        ]
                        .spacing(1),
                        fill(),
                    ]
                    .spacing(14)
                    .align_y(Alignment::Center);
                    if part.admin {
                        line = line.push(
                            container(text("Group admin").size(11.5).color(p.accent))
                                .padding([2, 8])
                                .style(theme::rounded(p.accent.scale_alpha(0.15), 6.0)),
                        );
                    }
                    let open: El<'a> = if me {
                        container(line).padding([6, 0]).into()
                    } else {
                        button(line)
                            .padding([6, 0])
                            .width(Length::Fill)
                            .style(theme::flat_button(p.text_primary))
                            .on_press(Message::Ui(Ui::OpenChat(part.jid.clone())))
                            .into()
                    };
                    members = members.push(open);
                }
            }
            Load::Loading => members = members.push(label(app, "Loading members…")),
            Load::Unavailable => {
                members = members.push(label(app, "Members are not available."));
            }
        }
        body = body.push(section(app, members.into()));
    } else {
        // Groups in common.
        let mut common = column![].spacing(2);
        match &info.common {
            Load::Ready(gs) if !gs.is_empty() => {
                common = common.push(label(app, format!("{} groups in common", gs.len())));
                for (jid, subject) in gs {
                    common = common.push(
                        button(
                            row![
                                avatar_for(app, jid, true, 40.0),
                                emoji_text(
                                    ellipsize(subject, 34),
                                    15.0,
                                    p.text_primary,
                                    INTER,
                                    false
                                ),
                            ]
                            .spacing(14)
                            .align_y(Alignment::Center),
                        )
                        .padding([6, 0])
                        .width(Length::Fill)
                        .style(theme::flat_button(p.text_primary))
                        .on_press(Message::Ui(Ui::OpenChat(jid.clone()))),
                    );
                }
            }
            Load::Ready(_) => common = common.push(label(app, "No groups in common")),
            Load::Loading => common = common.push(label(app, "Loading groups in common…")),
            Load::Unavailable => {
                common = common.push(label(app, "Groups in common are not available."));
            }
        }
        body = body.push(section(app, common.into()));

        // Block.
        let blocked = info.blocked.ready().copied();
        let short = app.chats.plain_name(&info.chat);
        let block: El<'a> = if info.confirm_block {
            let blocking = blocked != Some(true);
            column![
                text(if blocking {
                    format!("Block {short}? Blocked contacts cannot call you or send you messages.")
                } else {
                    format!("Unblock {short}?")
                })
                .size(14)
                .color(p.text_primary)
                .wrapping(Wrapping::Word),
                row![
                    fill(),
                    button(text("Cancel").size(14).color(p.accent))
                        .padding([8, 16])
                        .style(theme::link_button(t))
                        .on_press(Message::Ui(Ui::CancelBlock)),
                    button(
                        text(if blocking { "Block" } else { "Unblock" })
                            .size(14)
                            .color(p.accent_text)
                    )
                    .padding([8, 16])
                    .style(theme::primary_button(t))
                    .on_press(Message::Ui(Ui::SetBlocked(blocking))),
                ]
                .spacing(8),
            ]
            .spacing(12)
            .into()
        } else {
            let (txt, enabled) = match blocked {
                Some(true) => (format!("Unblock {short}"), true),
                Some(false) => (format!("Block {short}"), true),
                None => (format!("Block {short}"), false),
            };
            let b = button(
                row![
                    icon(Icon::Ban, 18.0, p.danger),
                    text(txt).size(15).color(p.danger),
                ]
                .spacing(16)
                .align_y(Alignment::Center),
            )
            .padding(0)
            .width(Length::Fill)
            .style(theme::flat_button(p.danger));
            if enabled {
                b.on_press(Message::Ui(Ui::AskBlock)).into()
            } else {
                b.into()
            }
        };
        body = body.push(section(app, block));
    }

    frame(
        app,
        column![
            header,
            scrollable(body.padding(iced::Padding::ZERO.bottom(16)))
                .direction(super::thin_scrollbar())
                .style(theme::scrollbar(t))
                .height(Length::Fill),
        ]
        .into(),
    )
}

fn frame<'a>(app: &'a App, content: El<'a>) -> El<'a> {
    let p = app.theme.palette;
    row![
        container(Space::new().width(1).height(Length::Fill)).style(theme::solid(p.border)),
        container(content)
            .width(WIDTH)
            .height(Length::Fill)
            .style(theme::solid(p.bg_panel)),
    ]
    .into()
}
