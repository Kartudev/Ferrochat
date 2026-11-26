//! "Forward message to": the chat picker of a forward (`app::forward`).

use iced::widget::text::Wrapping;
use iced::widget::{
    button, checkbox, column, container, opaque, row, scrollable, text, text_input,
};
use iced::{Alignment, Color, Length};

use super::emoji_font::emoji_text;
use super::icons::Icon;
use super::theme::{self, INTER, INTER_MEDIUM};
use super::{El, avatar_for, ellipsize, fill, icon};
use crate::app::forward::{self, FORWARD_SEARCH_ID, ForwardDialog, Fwd};
use crate::app::{App, Message, Ui};

/// Most chats listed at once (search narrows the rest).
const MAX_ROWS: usize = 300;

fn msg(f: Fwd) -> Message {
    Message::Ui(Ui::Forward(f))
}

/// The dialog, when open.
pub fn dialog(app: &App) -> Option<El<'_>> {
    let d = app.forward.as_ref()?;
    let t = &app.theme;
    let p = t.palette;
    let title = match d.msgs.len() {
        1 => "Forward message to".to_string(),
        n => format!("Forward {n} messages to"),
    };
    let header = row![
        button(icon(Icon::X, 20.0, p.text_secondary))
            .padding(6)
            .style(theme::icon_button(t, false))
            .on_press(msg(Fwd::Close)),
        text(title)
            .size(17)
            .font(INTER_MEDIUM)
            .color(p.text_primary),
    ]
    .spacing(14)
    .align_y(Alignment::Center);
    let search = container(
        row![
            icon(Icon::Search, 16.0, p.text_secondary),
            text_input("Search name or number", &d.query)
                .id(FORWARD_SEARCH_ID)
                .on_input(|q| msg(Fwd::Query(q)))
                .size(t.metrics.text)
                .padding([6, 0])
                .style(theme::input(t)),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding([2, 14])
    .style(theme::rounded(p.bg_input, 8.0));

    let found = forward::targets(&app.chats, &d.query);
    let mut list = column![].spacing(0);
    if found.is_empty() {
        list = list.push(
            container(text("No chats found").size(14).color(p.text_secondary))
                .center_x(Length::Fill)
                .padding([24, 0]),
        );
    }
    for e in found.into_iter().take(MAX_ROWS) {
        list = list.push(target_row(app, d, &e.meta.id, e.is_group()));
    }
    let note = if d.max_targets() == 1 {
        "Forwarded many times: one chat at a time"
    } else {
        "Recent chats"
    };
    let mut body = column![
        header,
        search,
        text(note).size(13).font(INTER_MEDIUM).color(p.accent),
        scrollable(list)
            .direction(super::thin_scrollbar())
            .style(theme::scrollbar(t))
            .height(Length::Fill),
    ]
    .spacing(14);
    if !d.targets.is_empty() {
        let names: Vec<String> = d.targets.iter().map(|c| app.chats.plain_name(c)).collect();
        body = body.push(
            container(
                row![
                    text(ellipsize(&names.join(", "), 60))
                        .size(14)
                        .color(p.text_primary)
                        .wrapping(Wrapping::None),
                    fill(),
                    button(icon(Icon::SendHorizontal, 20.0, p.accent_text))
                        .padding(11)
                        .style(theme::primary_button(t))
                        .on_press(msg(Fwd::Send)),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            )
            .padding([4, 0]),
        );
    }
    let height = (app.window_size().height - 80.0).clamp(300.0, 620.0);
    let card =
        container(body)
            .padding(20)
            .width(440)
            .height(height)
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

/// One chat: checkbox, avatar, name. Disabled once the limit is reached.
fn target_row<'a>(app: &'a App, d: &ForwardDialog, jid: &str, group: bool) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let ticked = d.has(jid);
    let enabled = ticked || !d.is_full();
    let mut check = checkbox(ticked).size(18).style(theme::checkbox(t));
    if enabled {
        let chat = jid.to_string();
        check = check.on_toggle(move |_| msg(Fwd::Target(chat.clone())));
    }
    let name_color = if enabled {
        p.text_primary
    } else {
        p.text_secondary
    };
    let line = row![
        check,
        avatar_for(app, jid, group, t.metrics.avatar_small),
        emoji_text(app.chats.display_name(jid), 15.5, name_color, INTER, false),
    ]
    .spacing(16)
    .align_y(Alignment::Center);
    let mut b = button(line)
        .width(Length::Fill)
        .padding([9, 8])
        .style(theme::chat_row(t, ticked));
    if enabled {
        b = b.on_press(msg(Fwd::Target(jid.to_string())));
    }
    b.into()
}
