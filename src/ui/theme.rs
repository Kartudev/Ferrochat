//! Palette, typography, spacing and widget styles. Single source of truth
//! for the look; no view hard-codes a colour or size.
//!
//! Values reproduce the WhatsApp Desktop / Web dark theme by eye (see the
//! palette table in `docs/architecture.md`). No assets of the official
//! client are used.

use iced::border::Radius;
use iced::widget::{button, container, scrollable, text_editor, text_input};
use iced::{Background, Border, Color, Font, Shadow, Theme, font};

/// Colour palette.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Window background.
    pub bg_app: Color,
    /// Chat list column.
    pub bg_sidebar: Color,
    /// Conversation background.
    pub bg_panel: Color,
    /// Headers, composer bar, popovers.
    pub bg_elevated: Color,
    /// Hovered row.
    pub bg_hover: Color,
    /// Selected row.
    pub bg_selected: Color,
    /// Main text.
    pub text_primary: Color,
    /// Previews, timestamps.
    pub text_secondary: Color,
    /// Placeholders, hints.
    pub text_muted: Color,
    /// Brand-like green accent (recreated).
    pub accent: Color,
    /// Text on accent.
    pub accent_text: Color,
    /// Links inside message bubbles.
    pub link: Color,
    /// Errors.
    pub danger: Color,
    /// Read ticks.
    pub success: Color,
    /// Separators.
    pub border: Color,
    /// Outgoing bubble.
    pub bubble_out: Color,
    /// Incoming bubble.
    pub bubble_in: Color,
    /// Unread badge.
    pub badge: Color,
    /// Muted unread badge.
    pub badge_muted: Color,
    /// Navigation rail.
    pub bg_rail: Color,
    /// Search field / chips.
    pub bg_input: Color,
    /// Active filter chip.
    pub chip_active: Color,
    /// Text on the active chip.
    pub chip_active_text: Color,
    /// Document / quote inner box in incoming bubbles.
    pub inset_in: Color,
    /// Same for outgoing bubbles.
    pub inset_out: Color,
    /// Date / system chips in the conversation.
    pub system_chip: Color,
    /// Encryption notice text.
    pub notice_text: Color,
    /// Secondary text inside outgoing bubbles.
    pub bubble_out_meta: Color,
    /// Default avatar background.
    pub avatar_bg: Color,
    /// Default avatar glyph.
    pub avatar_fg: Color,
}

const fn hex(rgb: u32) -> Color {
    Color::from_rgb(
        ((rgb >> 16) & 0xff) as f32 / 255.0,
        ((rgb >> 8) & 0xff) as f32 / 255.0,
        (rgb & 0xff) as f32 / 255.0,
    )
}

/// Dark palette (default).
pub const DARK: Palette = Palette {
    bg_app: hex(0x0c1317),
    bg_sidebar: hex(0x111b21),
    bg_panel: hex(0x0b141a),
    bg_elevated: hex(0x202c33),
    bg_hover: hex(0x202c33),
    bg_selected: hex(0x2a3942),
    text_primary: hex(0xe9edef),
    text_secondary: hex(0x8696a0),
    text_muted: hex(0x667781),
    accent: hex(0x00a884),
    accent_text: hex(0x111b21),
    link: hex(0x53bdeb),
    danger: hex(0xf15c6d),
    success: hex(0x53bdeb),
    border: hex(0x222d34),
    bubble_out: hex(0x005c4b),
    bubble_in: hex(0x202c33),
    badge: hex(0x00a884),
    badge_muted: hex(0x8696a0),
    bg_rail: hex(0x202c33),
    bg_input: hex(0x202c33),
    chip_active: hex(0x0a332c),
    chip_active_text: hex(0x00a884),
    inset_in: hex(0x1d282f),
    inset_out: hex(0x025144),
    system_chip: hex(0x182229),
    notice_text: hex(0xffd279),
    bubble_out_meta: hex(0x8fbfb2),
    avatar_bg: hex(0x6a7175),
    avatar_fg: hex(0xcfd4d6),
};

/// Light palette.
pub const LIGHT: Palette = Palette {
    bg_app: hex(0xdfe3e5),
    bg_sidebar: hex(0xffffff),
    bg_panel: hex(0xefeae2),
    bg_elevated: hex(0xf0f2f5),
    bg_hover: hex(0xf5f6f6),
    bg_selected: hex(0xf0f2f5),
    text_primary: hex(0x111b21),
    text_secondary: hex(0x667781),
    text_muted: hex(0x8696a0),
    accent: hex(0x00a884),
    accent_text: hex(0xffffff),
    link: hex(0x027eb5),
    danger: hex(0xea0038),
    success: hex(0x53bdeb),
    border: hex(0xe9edef),
    bubble_out: hex(0xd9fdd3),
    bubble_in: hex(0xffffff),
    badge: hex(0x25d366),
    badge_muted: hex(0x8696a0),
    bg_rail: hex(0xf0f2f5),
    bg_input: hex(0xf0f2f5),
    chip_active: hex(0xe7fce3),
    chip_active_text: hex(0x008069),
    inset_in: hex(0xf5f6f6),
    inset_out: hex(0xd1f4cc),
    system_chip: hex(0xffffff),
    notice_text: hex(0x54656f),
    bubble_out_meta: hex(0x667781),
    avatar_bg: hex(0xdfe5e7),
    avatar_fg: hex(0xffffff),
};

/// Sizes and spacing.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    /// Navigation rail width.
    pub rail_width: f32,
    /// Chat list width.
    pub sidebar_width: f32,
    /// Chat row height.
    pub row_height: f32,
    /// Avatar in chat rows.
    pub avatar: f32,
    /// Avatar in the conversation header.
    pub avatar_small: f32,
    /// Header height.
    pub header_height: f32,
    /// Max bubble width.
    pub bubble_max: f32,
    /// Bubble corner radius.
    pub bubble_radius: f32,
    /// Base text size.
    pub text: f32,
    /// Small text (timestamps, previews).
    pub text_small: f32,
    /// Tiny text (bubble time).
    pub text_tiny: f32,
    /// Titles.
    pub text_title: f32,
    /// Standard icon size.
    pub icon: f32,
}

/// Default metrics.
pub const METRICS: Metrics = Metrics {
    rail_width: 64.0,
    sidebar_width: 420.0,
    row_height: 72.0,
    avatar: 49.0,
    avatar_small: 40.0,
    header_height: 59.0,
    bubble_max: 520.0,
    bubble_radius: 7.5,
    text: 14.5,
    text_small: 13.0,
    text_tiny: 11.0,
    text_title: 22.0,
    icon: 22.0,
};

/// Theme bundle.
#[derive(Debug, Clone, Copy)]
pub struct FerroTheme {
    /// Colours.
    pub palette: Palette,
    /// Sizes.
    pub metrics: Metrics,
    /// Dark?
    pub dark: bool,
}

impl FerroTheme {
    /// Dark or light.
    pub fn new(dark: bool) -> Self {
        Self {
            palette: if dark { DARK } else { LIGHT },
            metrics: METRICS,
            dark,
        }
    }

    /// The iced base theme.
    pub fn iced(&self) -> Theme {
        let p = &self.palette;
        Theme::custom(
            if self.dark {
                "Ferrochat Dark"
            } else {
                "Ferrochat Light"
            }
            .to_string(),
            iced::theme::Palette {
                background: p.bg_app,
                text: p.text_primary,
                primary: p.accent,
                success: p.accent,
                warning: p.notice_text,
                danger: p.danger,
            },
        )
    }
}

/// Inter (SIL OFL 1.1), embedded.
pub const FONT_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
/// Inter Medium.
pub const FONT_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
/// Inter SemiBold.
pub const FONT_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

/// Twemoji (graphics CC-BY 4.0 by Twitter and jdecked/twemoji, COLRv0 build
/// Apache-2.0 by Mozilla / Sable), renamed "Ferrochat Twemoji" so no system
/// face can shadow it. Loaded zero-copy by `emoji_font::install`.
pub const TWEMOJI: &[u8] = include_bytes!("../../assets/fonts/Twemoji.ttf");

/// Body font.
pub const INTER: Font = Font::with_name("Inter");
/// Medium weight.
pub const INTER_MEDIUM: Font = Font {
    weight: font::Weight::Medium,
    ..INTER
};
/// Semibold weight.
pub const INTER_SEMIBOLD: Font = Font {
    weight: font::Weight::Semibold,
    ..INTER
};

fn border(radius: f32) -> Border {
    Border {
        radius: Radius::from(radius),
        width: 0.0,
        color: Color::TRANSPARENT,
    }
}

/// Solid background container.
pub fn solid(color: Color) -> impl Fn(&Theme) -> container::Style + use<> {
    move |_| container::Style {
        background: Some(Background::Color(color)),
        ..Default::default()
    }
}

/// Rounded solid container.
pub fn rounded(color: Color, radius: f32) -> impl Fn(&Theme) -> container::Style + use<> {
    move |_| container::Style {
        background: Some(Background::Color(color)),
        border: border(radius),
        ..Default::default()
    }
}

/// Right / bottom hairline separator container.
pub fn bordered(bg: Color, line: Color) -> impl Fn(&Theme) -> container::Style + use<> {
    move |_| container::Style {
        background: Some(Background::Color(bg)),
        border: Border {
            radius: Radius::from(0.0),
            width: 1.0,
            color: line,
        },
        ..Default::default()
    }
}

/// Message bubble. The corner next to the tail is square.
pub fn bubble(
    t: &FerroTheme,
    outgoing: bool,
    tail: bool,
) -> impl Fn(&Theme) -> container::Style + use<> {
    let p = t.palette;
    let r = t.metrics.bubble_radius;
    move |_| {
        let mut radius = Radius::from(r);
        if tail {
            if outgoing {
                radius.top_right = 0.0;
            } else {
                radius.top_left = 0.0;
            }
        }
        container::Style {
            text_color: Some(p.text_primary),
            background: Some(Background::Color(if outgoing {
                p.bubble_out
            } else {
                p.bubble_in
            })),
            border: Border {
                radius,
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            // No `Shadow`: the tiny-skia renderer (the default backend)
            // computes a shadow by evaluating a rounded-box SDF for every
            // pixel of the shadow box into a fresh pixmap, for every bubble on
            // every frame. While scrolling that was most of the frame time.
            // `bubble_shadow` draws the same 1 px drop as a plain quad.
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// The bubble's 1 px drop shadow as a second quad behind it (cheap on every
/// renderer), like the official `0 1px 0.5px rgba(11,20,26,.13)`.
pub fn bubble_shadow(
    t: &FerroTheme,
    outgoing: bool,
    tail: bool,
) -> impl Fn(&Theme) -> container::Style + use<> {
    let r = t.metrics.bubble_radius;
    move |_| {
        let mut radius = Radius::from(r);
        if tail {
            if outgoing {
                radius.top_right = 0.0;
            } else {
                radius.top_left = 0.0;
            }
        }
        container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.13))),
            border: Border {
                radius,
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            ..container::Style::default()
        }
    }
}

/// Chat list row.
pub fn chat_row(
    t: &FerroTheme,
    selected: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style + use<> {
    let p = t.palette;
    move |_, status| {
        let bg = if selected {
            Some(Background::Color(p.bg_selected))
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(Background::Color(p.bg_hover))
        } else {
            None
        };
        button::Style {
            background: bg,
            text_color: p.text_primary,
            border: border(0.0),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Transparent icon button with a round hover.
pub fn icon_button(
    t: &FerroTheme,
    active: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style + use<> {
    let p = t.palette;
    move |_, status| button::Style {
        background: if active || matches!(status, button::Status::Hovered | button::Status::Pressed)
        {
            Some(Background::Color(Color {
                a: 0.12,
                ..p.text_primary
            }))
        } else {
            None
        },
        text_color: p.text_secondary,
        border: border(50.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Filled accent button (send, primary actions).
pub fn primary_button(t: &FerroTheme) -> impl Fn(&Theme, button::Status) -> button::Style + use<> {
    let p = t.palette;
    move |_, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => Color {
                a: 0.85,
                ..p.accent
            },
            _ => p.accent,
        })),
        text_color: p.accent_text,
        border: border(24.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Round filled call button (accept green, decline / hang up red, or the
/// neutral grey of mute).
pub fn call_button(bg: Color) -> impl Fn(&Theme, button::Status) -> button::Style + use<> {
    move |_, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => Color { a: 0.85, ..bg },
            _ => bg,
        })),
        text_color: Color::WHITE,
        border: border(CALL_BUTTON / 2.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Diameter of the call panel's round buttons.
pub const CALL_BUTTON: f32 = 52.0;
/// Width of the call panel.
pub const CALL_PANEL_WIDTH: f32 = 320.0;
/// Avatar size in the call panel.
pub const CALL_AVATAR: f32 = 96.0;

/// Plain text link button.
pub fn link_button(t: &FerroTheme) -> impl Fn(&Theme, button::Status) -> button::Style + use<> {
    let p = t.palette;
    move |_, status| button::Style {
        background: None,
        text_color: match status {
            button::Status::Hovered => p.text_primary,
            _ => p.accent,
        },
        border: border(0.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Filter chip.
pub fn chip(
    t: &FerroTheme,
    active: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style + use<> {
    let p = t.palette;
    move |_, status| button::Style {
        background: Some(Background::Color(if active {
            p.chip_active
        } else if matches!(status, button::Status::Hovered) {
            p.bg_selected
        } else {
            p.bg_input
        })),
        text_color: if active {
            p.chip_active_text
        } else {
            p.text_secondary
        },
        border: border(16.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Switch like the official client's settings toggles (accent when on).
pub fn toggler(
    t: &FerroTheme,
) -> impl Fn(&Theme, iced::widget::toggler::Status) -> iced::widget::toggler::Style + use<> {
    let p = t.palette;
    move |_, status| {
        let on = matches!(
            status,
            iced::widget::toggler::Status::Active { is_toggled: true }
                | iced::widget::toggler::Status::Hovered { is_toggled: true }
        );
        iced::widget::toggler::Style {
            background: Background::Color(if on { p.accent } else { p.bg_selected }),
            background_border_width: 1.0,
            background_border_color: if on { p.accent } else { p.text_muted },
            foreground: Background::Color(if on { p.accent_text } else { p.text_secondary }),
            foreground_border_width: 0.0,
            foreground_border_color: Color::TRANSPARENT,
            text_color: Some(p.text_primary),
            border_radius: None,
            padding_ratio: 0.2,
        }
    }
}

/// Radio buttons in dialogs (accent dot when picked).
pub fn radio(
    t: &FerroTheme,
) -> impl Fn(&Theme, iced::widget::radio::Status) -> iced::widget::radio::Style + use<> {
    let p = t.palette;
    move |_, status| {
        let picked = matches!(
            status,
            iced::widget::radio::Status::Active { is_selected: true }
                | iced::widget::radio::Status::Hovered { is_selected: true }
        );
        iced::widget::radio::Style {
            background: Background::Color(Color::TRANSPARENT),
            dot_color: p.accent,
            border_width: 2.0,
            border_color: if picked { p.accent } else { p.text_secondary },
            text_color: Some(p.text_primary),
        }
    }
}

/// Checkboxes in dialogs (accent box when ticked).
pub fn checkbox(
    t: &FerroTheme,
) -> impl Fn(&Theme, iced::widget::checkbox::Status) -> iced::widget::checkbox::Style + use<> {
    let p = t.palette;
    move |_, status| {
        let checked = match status {
            iced::widget::checkbox::Status::Active { is_checked }
            | iced::widget::checkbox::Status::Hovered { is_checked }
            | iced::widget::checkbox::Status::Disabled { is_checked } => is_checked,
        };
        iced::widget::checkbox::Style {
            background: Background::Color(if checked {
                p.accent
            } else {
                Color::TRANSPARENT
            }),
            icon_color: p.accent_text,
            border: iced::Border {
                color: if checked { p.accent } else { p.text_secondary },
                width: 2.0,
                radius: 3.0.into(),
            },
            text_color: Some(p.text_primary),
        }
    }
}

/// Bubble-embedded button (play, document).
pub fn flat_button(color: Color) -> impl Fn(&Theme, button::Status) -> button::Style + use<> {
    move |_, _| button::Style {
        background: None,
        text_color: color,
        border: border(8.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Search / composer input (the container provides the rounded background).
pub fn input(t: &FerroTheme) -> impl Fn(&Theme, text_input::Status) -> text_input::Style + use<> {
    let p = t.palette;
    move |_, _| text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: border(0.0),
        icon: p.text_secondary,
        placeholder: p.text_secondary,
        value: p.text_primary,
        selection: Color { a: 0.4, ..p.accent },
    }
}

/// The composer's multi-line editor (the container provides the background).
pub fn editor(
    t: &FerroTheme,
) -> impl Fn(&Theme, text_editor::Status) -> text_editor::Style + use<> {
    let p = t.palette;
    move |_, _| text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: border(0.0),
        placeholder: p.text_secondary,
        value: p.text_primary,
        selection: Color { a: 0.4, ..p.accent },
    }
}

/// Thin scrollbars like the official client.
pub fn scrollbar(
    t: &FerroTheme,
) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style + use<> {
    let p = t.palette;
    move |theme, status| {
        let mut s = scrollable::default(theme, status);
        let rail = scrollable::Rail {
            background: None,
            border: border(0.0),
            scroller: scrollable::Scroller {
                background: Background::Color(Color {
                    a: 0.25,
                    ..p.text_secondary
                }),
                border: border(3.0),
            },
        };
        s.vertical_rail = rail;
        s.horizontal_rail = rail;
        s.container = container::Style::default();
        s
    }
}

/// Deterministic accent colour for sender names in groups.
pub fn sender_color(jid: &str) -> Color {
    const COLORS: [u32; 8] = [
        0x53bdeb, 0xfc9775, 0xa5b337, 0xd88deb, 0x7f66ff, 0x25d366, 0xffd279, 0xff72a1,
    ];
    let h = jid
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    hex(COLORS[(h % COLORS.len() as u32) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_have_contrast() {
        // Luminance difference between text and background stays large.
        fn lum(c: Color) -> f32 {
            0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
        }
        for p in [DARK, LIGHT] {
            assert!((lum(p.text_primary) - lum(p.bg_sidebar)).abs() > 0.6);
            assert!((lum(p.text_primary) - lum(p.bubble_out)).abs() > 0.5);
        }
    }

    #[test]
    fn sender_colors_are_stable() {
        assert_eq!(
            sender_color("a@s.whatsapp.net"),
            sender_color("a@s.whatsapp.net")
        );
    }

    /// tiny-skia renders a `Shadow` with a per-pixel SDF into a new pixmap
    /// for every quad on every frame: bubbles must not use one (a third of
    /// the scroll frame time before; see `app::perf`).
    #[test]
    fn bubbles_have_no_renderer_shadow() {
        let t = FerroTheme::new(true);
        for out in [false, true] {
            for tail in [false, true] {
                let s = bubble(&t, out, tail)(&Theme::Dark);
                assert_eq!(s.shadow.color.a, 0.0);
                let sh = bubble_shadow(&t, out, tail)(&Theme::Dark);
                assert_eq!(sh.shadow.color.a, 0.0);
            }
        }
    }
}
