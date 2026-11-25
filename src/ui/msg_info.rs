//! The "Message info" panel (`app::msg_info`) on the right of the
//! conversation, where the contact info drawer goes, like WhatsApp Desktop.

use iced::widget::text::Wrapping;
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Length};

use super::emoji_font::emoji_text;
use super::icons::Icon;
use super::theme::{self, INTER};
use super::{El, avatar_for, clock, icon, local_day, ticks};
use crate::app::msg_info::{Line, Summary};
use crate::app::{App, Message, Ui};
use crate::net::{Message as ChatMessage, ReceiptStatus};

/// "Today at 14:05", "Yesterday at 09:12", "Monday at …", "03/11/2026 at …".
pub fn when(ts: i64, now: i64) -> String {
    let day = match local_day(now) - local_day(ts) {
        0 => "Today".to_string(),
        _ => super::list_time(ts, now),
    };
    format!("{day} at {}", clock(ts))
}

fn section<'a>(app: &'a App, body: El<'a>) -> El<'a> {
    container(body)
        .padding([14, 24])
        .width(Length::Fill)
        .style(theme::solid(app.theme.palette.bg_elevated))
        .into()
}

/// A step of a direct chat: ticks, label and time (or a dash).
fn step<'a>(
    app: &'a App,
    status: ReceiptStatus,
    label: &'static str,
    line: Option<&Line>,
) -> El<'a> {
    let p = app.theme.palette;
    let now = chrono::Utc::now().timestamp();
    let at = match line.and_then(|l| l.at) {
        Some(ts) => when(ts, now),
        None if line.is_some() => "Time not recorded".into(),
        None => "—".into(),
    };
    section(
        app,
        column![
            row![
                ticks(&app.theme, status, 18.0),
                text(label).size(15).color(p.text_primary)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            text(at).size(13.5).color(p.text_secondary),
        ]
        .spacing(6)
        .into(),
    )
}

/// A group list: title with ticks, then one row per member.
fn people<'a>(app: &'a App, status: ReceiptStatus, title: &'static str, lines: &[Line]) -> El<'a> {
    let p = app.theme.palette;
    let now = chrono::Utc::now().timestamp();
    let mut col = column![
        row![
            ticks(&app.theme, status, 18.0),
            text(title).size(15).color(p.text_primary)
        ]
        .spacing(10)
        .align_y(Alignment::Center)
    ]
    .spacing(12);
    if lines.is_empty() {
        col = col.push(text("Nobody yet").size(13.5).color(p.text_secondary));
    }
    for l in lines {
        let name = app.chats.display_name(&l.participant);
        let at = l.at.map(|ts| when(ts, now)).unwrap_or_default();
        col = col.push(
            row![
                avatar_for(app, &l.participant, false, 40.0),
                column![
                    emoji_text(name, 15.0, p.text_primary, INTER, false),
                    text(at)
                        .size(13)
                        .color(p.text_secondary)
                        .wrapping(Wrapping::None),
                ]
                .spacing(2),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }
    section(app, col.into())
}

fn body<'a>(app: &'a App, s: &Summary, group: bool) -> El<'a> {
    let p = app.theme.palette;
    let mut col = column![].spacing(10).width(Length::Fill);
    if group {
        if !s.played.is_empty() {
            col = col.push(people(app, ReceiptStatus::Played, "Played by", &s.played));
        }
        col = col.push(people(app, ReceiptStatus::Read, "Read by", &s.read));
        col = col.push(people(
            app,
            ReceiptStatus::Delivered,
            "Delivered to",
            &s.delivered,
        ));
        if let Some(n) = s.waiting.filter(|n| *n > 0) {
            col = col.push(section(
                app,
                text(format!("{n} remaining"))
                    .size(13.5)
                    .color(p.text_secondary)
                    .into(),
            ));
        }
    } else {
        if !s.played.is_empty() {
            col = col.push(step(app, ReceiptStatus::Played, "Played", s.played.first()));
        }
        col = col.push(step(app, ReceiptStatus::Read, "Read", s.read.first()));
        col = col.push(step(
            app,
            ReceiptStatus::Delivered,
            "Delivered",
            s.delivered.first(),
        ));
    }
    col.into()
}

/// The panel for `msg` (one of ours, in the open chat).
pub fn view<'a>(app: &'a App, msg: &'a ChatMessage) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let m = t.metrics;
    let group = crate::net::is_group_jid(&msg.chat);
    let header = container(
        row![
            button(icon(Icon::X, 22.0, p.text_secondary))
                .padding(8)
                .style(theme::icon_button(t, false))
                .on_press(Message::Ui(Ui::CloseMessageInfo)),
            text("Message info").size(16).color(p.text_primary)
        ]
        .spacing(16)
        .align_y(Alignment::Center),
    )
    .height(m.header_height)
    .padding([0, 16])
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .style(theme::solid(p.bg_elevated));

    // The message itself, as a small outgoing bubble.
    let bubble = container(
        column![
            emoji_text(app.preview(msg), m.text, p.text_primary, INTER, true),
            row![
                Space::new().width(Length::Fill),
                text(clock(msg.timestamp))
                    .size(11.5)
                    .color(p.bubble_out_meta),
                ticks(t, msg.status, 15.0),
            ]
            .spacing(4)
            .align_y(Alignment::Center),
        ]
        .spacing(4),
    )
    .padding([8, 10])
    .max_width(300)
    .style(theme::rounded(p.bubble_out, 8.0));
    let top = container(row![Space::new().width(Length::Fill), bubble])
        .padding([24, 24])
        .width(Length::Fill)
        .style(theme::solid(p.bg_panel));

    let content: El<'a> = match app.msg_info_summary() {
        Some(s) => body(app, &s, group),
        None => section(
            app,
            text("Loading…").size(13.5).color(p.text_secondary).into(),
        ),
    };
    let sent = section(
        app,
        column![
            row![
                ticks(t, ReceiptStatus::Sent, 18.0),
                text("Sent").size(15).color(p.text_primary)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            text(when(msg.timestamp, chrono::Utc::now().timestamp()))
                .size(13.5)
                .color(p.text_secondary),
        ]
        .spacing(6)
        .into(),
    );
    let panel = column![
        header,
        scrollable(column![top, content, sent].spacing(10))
            .direction(super::thin_scrollbar())
            .style(theme::scrollbar(t))
    ];
    row![
        container(Space::new().width(1).height(Length::Fill)).style(theme::solid(p.border)),
        container(panel)
            .width(super::info::WIDTH)
            .height(Length::Fill)
            .style(theme::solid(p.bg_panel)),
    ]
    .into()
}
